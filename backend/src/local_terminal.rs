use crate::errors::{AppError, AppResult};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::ipc::{Channel as IpcChannel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

pub struct LocalTerminalRegistry {
    inner: AsyncMutex<HashMap<String, Arc<LocalTerminalSession>>>,
}

impl LocalTerminalRegistry {
    pub fn new() -> Self {
        Self {
            inner: AsyncMutex::new(HashMap::new()),
        }
    }

    pub async fn insert(&self, session: LocalTerminalSession) -> String {
        let id = session.id.clone();
        self.inner
            .lock()
            .await
            .insert(id.clone(), Arc::new(session));
        id
    }

    pub async fn get(&self, id: &str) -> Option<Arc<LocalTerminalSession>> {
        self.inner.lock().await.get(id).cloned()
    }

    pub async fn remove(&self, id: &str) -> Option<Arc<LocalTerminalSession>> {
        self.inner.lock().await.remove(id)
    }
}

pub struct LocalTerminalSession {
    pub id: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    output_channel: Arc<RwLock<Option<IpcChannel<InvokeResponseBody>>>>,
    shutting_down: Arc<AtomicBool>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
    cwd_thread: Mutex<Option<JoinHandle<()>>>,
}

impl LocalTerminalSession {
    pub fn start(
        app: AppHandle,
        id: String,
        cols: u16,
        rows: u16,
        cwd: Option<PathBuf>,
    ) -> AppResult<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Ssh(format!("local pty open: {e}")))?;

        let shell = pick_local_shell();
        let mut cmd = CommandBuilder::new(shell.clone());
        cmd.env("SHELL", &shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("PATH", terminal_like_path());
        // GUI-launched macOS apps often start without a UTF-8 locale. zsh's
        // line editor then treats newer emoji as unprintable and renders them
        // as <0001f972>-style placeholders. A real terminal starts shells with
        // a UTF-8 locale, so make that explicit for local PTYs.
        if !env_locale_is_utf8("LANG") {
            cmd.env("LANG", "en_US.UTF-8");
        }
        if !env_locale_is_utf8("LC_CTYPE") {
            cmd.env("LC_CTYPE", "en_US.UTF-8");
        }
        if !env_locale_is_utf8("LC_ALL") {
            cmd.env_remove("LC_ALL");
        }
        if shell.ends_with("/bash") {
            // macOS' system zsh displays newer emoji as <0001f972>-style
            // placeholders while editing a command line. When no modern zsh
            // is available yet, bash is the local shell that preserves the
            // user's typed text correctly; give it a clean terminal-like
            // prompt instead of the raw "bash-3.2$" default.
            cmd.env("PS1", "\\u@\\h \\W \\$ ");
        }
        // Tersh is often launched through npm/tauri dev or a GUI wrapper. Do
        // not leak app/process env into the user's interactive shell; the
        // shell should start like a normal terminal, not as a child of the
        // package manager or a stale local proxy.
        for (key, _) in std::env::vars() {
            let lower = key.to_ascii_lowercase();
            if lower == "npm_config_prefix"
                || lower.starts_with("npm_config_")
                || lower.starts_with("npm_package_")
                || lower.starts_with("npm_lifecycle_")
                || lower == "npm_command"
                || lower == "npm_execpath"
                || lower == "npm_node_execpath"
                || lower == "http_proxy"
                || lower == "https_proxy"
                || lower == "all_proxy"
                || lower == "no_proxy"
            {
                cmd.env_remove(key);
            }
        }
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        } else if let Ok(home) = std::env::var("HOME") {
            cmd.cwd(home);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::Ssh(format!("local shell start: {e}")))?;
        let child_pid = child.process_id();
        drop(pair.slave);

        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(e) => {
                terminate_child(child.as_mut());
                return Err(AppError::Ssh(format!("local pty reader: {e}")));
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(e) => {
                terminate_child(child.as_mut());
                return Err(AppError::Ssh(format!("local pty writer: {e}")));
            }
        };
        let output_channel: Arc<RwLock<Option<IpcChannel<InvokeResponseBody>>>> =
            Arc::new(RwLock::new(None));
        let output_for_thread = output_channel.clone();
        let shutting_down = Arc::new(AtomicBool::new(false));
        let shutdown_for_reader = shutting_down.clone();
        let id_for_thread = id.clone();
        let app_for_thread = app.clone();

        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while !shutdown_for_reader.load(Ordering::Acquire) {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                if shutdown_for_reader.load(Ordering::Acquire) {
                    break;
                }
                let bytes = buf[..n].to_vec();
                if let Ok(guard) = output_for_thread.read() {
                    if let Some(channel) = guard.as_ref() {
                        if channel.send(InvokeResponseBody::Raw(bytes.clone())).is_ok() {
                            continue;
                        }
                    }
                }
                let _ = app_for_thread
                    .emit(&format!("ssh://{id_for_thread}/out"), base64_encode(&bytes));
            }
            let _ = app_for_thread.emit(&format!("ssh://{id_for_thread}/close"), ());
        });

        let cwd_thread = child_pid.map(|pid| {
            let id_for_cwd = id.clone();
            let app_for_cwd = app.clone();
            let shutdown_for_cwd = shutting_down.clone();
            std::thread::spawn(move || {
                let mut last: Option<PathBuf> = None;
                while !shutdown_for_cwd.load(Ordering::Acquire) {
                    let Some(cwd) = process_cwd(pid) else {
                        break;
                    };
                    if last.as_ref() != Some(&cwd) {
                        last = Some(cwd.clone());
                        let _ = app_for_cwd.emit(
                            &format!("local-terminal://{id_for_cwd}/cwd"),
                            cwd.to_string_lossy().to_string(),
                        );
                    }
                    sleep_until_shutdown(&shutdown_for_cwd, Duration::from_secs(5));
                }
            })
        });

        Ok(Self {
            id,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            output_channel,
            shutting_down,
            reader_thread: Mutex::new(Some(reader_thread)),
            cwd_thread: Mutex::new(cwd_thread),
        })
    }

    pub fn bind_output_channel(&self, channel: IpcChannel<InvokeResponseBody>) {
        if let Ok(mut guard) = self.output_channel.write() {
            *guard = Some(channel);
        }
    }

    pub fn send(&self, bytes: Vec<u8>) -> AppResult<()> {
        self.writer
            .lock()
            .map_err(|_| AppError::Ssh("local terminal writer lock poisoned".into()))?
            .write_all(&bytes)
            .map_err(|e| AppError::Ssh(format!("local terminal write: {e}")))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> AppResult<()> {
        self.master
            .lock()
            .map_err(|_| AppError::Ssh("local terminal master lock poisoned".into()))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::Ssh(format!("local terminal resize: {e}")))
    }

    pub fn disconnect(&self) -> AppResult<()> {
        self.shutdown();
        self.join_threads();
        Ok(())
    }

    fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut child) = self.child.lock() {
            terminate_child(child.as_mut());
        }
    }

    fn join_threads(&self) {
        let current_thread = std::thread::current().id();
        if let Ok(mut thread) = self.reader_thread.lock() {
            if let Some(thread) = thread.take() {
                if thread.thread().id() != current_thread {
                    let _ = thread.join();
                }
            }
        }
        if let Ok(mut thread) = self.cwd_thread.lock() {
            if let Some(thread) = thread.take() {
                if thread.thread().id() != current_thread {
                    let _ = thread.join();
                }
            }
        }
    }
}

impl Drop for LocalTerminalSession {
    fn drop(&mut self) {
        self.shutdown();
        self.join_threads();
    }
}

fn terminate_child(child: &mut dyn Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn env_locale_is_utf8(key: &str) -> bool {
    std::env::var(key)
        .map(|value| {
            let normalized = value.to_ascii_uppercase();
            normalized.contains("UTF-8") || normalized.contains("UTF8")
        })
        .unwrap_or(false)
}

fn terminal_like_path() -> String {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    if let Ok(path) = std::env::var("PATH") {
        for entry in path.split(':') {
            push_path_entry(&mut entries, &mut seen, entry);
        }
    }

    for entry in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        push_path_entry(&mut entries, &mut seen, entry);
    }

    if let Ok(home) = std::env::var("HOME") {
        for suffix in [".local/bin", ".cargo/bin", ".bun/bin", "Library/pnpm"] {
            let entry = Path::new(&home).join(suffix);
            push_path_entry(&mut entries, &mut seen, &entry.to_string_lossy());
        }
    }

    entries.join(":")
}

fn push_path_entry(entries: &mut Vec<String>, seen: &mut HashSet<String>, entry: &str) {
    let entry = entry.trim();
    if entry.is_empty() || !seen.insert(entry.to_string()) {
        return;
    }
    entries.push(entry.to_string());
}

fn pick_local_shell() -> String {
    let env_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

    for candidate in bundled_or_modern_zsh_candidates() {
        if executable_exists(&candidate) {
            return candidate;
        }
    }

    #[cfg(target_os = "macos")]
    {
        if env_shell == "/bin/zsh" || env_shell == "/usr/bin/zsh" {
            return "/bin/bash".to_string();
        }
    }

    env_shell
}

fn bundled_or_modern_zsh_candidates() -> Vec<String> {
    let mut candidates = Vec::new();

    #[cfg(target_os = "macos")]
    if let Ok(exe) = std::env::current_exe() {
        if let Some(contents_dir) = exe.parent().and_then(|macos_dir| macos_dir.parent()) {
            candidates.push(
                contents_dir
                    .join("Resources")
                    .join("bin")
                    .join("zsh")
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    candidates.push("/opt/homebrew/bin/zsh".to_string());
    candidates.push("/usr/local/bin/zsh".to_string());
    candidates
}

fn executable_exists(path: &str) -> bool {
    let path = Path::new(path);
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn sleep_until_shutdown(shutting_down: &AtomicBool, duration: Duration) {
    let mut slept = Duration::ZERO;
    while slept < duration && !shutting_down.load(Ordering::Acquire) {
        let step = Duration::from_millis(100);
        std::thread::sleep(step);
        slept += step;
    }
}

fn process_cwd(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix('n'))
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
    }
}
