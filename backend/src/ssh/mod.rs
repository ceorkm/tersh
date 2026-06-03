use crate::errors::{AppError, AppResult};
use crate::vault::Vault;
use async_trait::async_trait;
use russh::client::{self, Handle, Handler, Msg};
use russh::keys::{decode_secret_key, key::PublicKey};
use russh::{Channel, ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::ipc::{Channel as IpcChannel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex, Notify, RwLock};
use zeroize::Zeroizing;

pub mod auth;

/// Map of (remote_bind_host, remote_bind_port) -> (local_host, local_port).
/// Used by ClientHandler::server_channel_open_forwarded_tcpip to route incoming
/// reverse-tunnel channels to the configured local destination.
pub type ForwardMap = Arc<RwLock<HashMap<(String, u32), (String, u16)>>>;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub cols: u16,
    pub rows: u16,
    /// Vault host_id — used to scope known-host fingerprint lookups.
    pub host_id: String,
    /// Environment variables to request via SSH SetEnv before shell start.
    pub env_vars: Vec<(String, String)>,
    /// Command to send into the shell after it opens (no auto-Enter — we add \n).
    pub startup_snippet: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMethod {
    Password {
        password: String,
    },
    KeyFile {
        path: String,
        passphrase: Option<String>,
    },
}

type CachedAgent = Arc<RwLock<Option<Option<(crate::agent_detect::AgentKind, u32)>>>>;

pub struct SshSession {
    pub id: String,
    handle: Arc<Mutex<Handle<ClientHandler>>>,
    /// User input → shell channel
    input_tx: mpsc::UnboundedSender<ShellMsg>,
    forwards: ForwardMap,
    output_channel: Arc<RwLock<Option<IpcChannel<InvokeResponseBody>>>>,
    /// Cached agent detection result (kind, pid). Detect runs `ps` on the
    /// remote which holds the SSH handle lock — caching avoids that overhead
    /// on every drag-drop upload. Outer Option = "have we tried yet";
    /// inner Option = "did we find anything".
    cached_agent: CachedAgent,
    /// One long-lived SFTP subsystem per SSH session. Opening an SFTP subsystem
    /// negotiates a new SSH channel; doing that for every folder click makes the
    /// file browser feel broken. Keep it cached like mature SSH clients do.
    cached_sftp: Arc<Mutex<Option<Arc<SftpSession>>>>,
    /// Active SFTP-operation refcount. invalidate_sftp() waits for this to hit
    /// 0 before closing the cached session, so concurrent ops never pull the
    /// rug out from under each other (which was hanging the file browser when
    /// upload + list-dir happened to overlap on the same session).
    sftp_active_ops: Arc<AtomicUsize>,
    sftp_ops_idle: Arc<Notify>,
    /// Serializes SFTP acquisition vs invalidation. Without this gate,
    /// invalidate_sftp could observe zero active ops, then close the cached
    /// subsystem while a new acquire was between "clone cached session" and
    /// "mark active".
    sftp_lifecycle: Arc<Mutex<()>>,
    /// True when disconnect() was called explicitly. The channel pump uses
    /// this to decide whether to emit `ssh://<sid>/close` (intentional) or
    /// `ssh://<sid>/disconnected` (network drop, server reboot, etc.) — the
    /// frontend treats only the latter as eligible for auto-reconnect.
    intentional_close: Arc<AtomicBool>,
    /// Handle to the channel-pump task. Held so disconnect() can abort the
    /// pump deterministically rather than waiting for input_tx to drop.
    /// Without this, a wedged `channel.wait()` could keep the pump alive
    /// indefinitely after the session is removed from the registry.
    pump_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

pub struct SftpOnlySession {
    handle: Handle<ClientHandler>,
    sftp: SftpSession,
}

impl SftpOnlySession {
    pub fn sftp(&self) -> &SftpSession {
        &self.sftp
    }

    pub async fn disconnect(self) -> AppResult<()> {
        let _ = self.sftp.close().await;
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "transfer complete", "")
            .await;
        Ok(())
    }
}

enum ShellMsg {
    Data(Vec<u8>),
    Resize {
        cols: u32,
        rows: u32,
        ack: oneshot::Sender<Result<(), String>>,
    },
}

impl SshSession {
    /// Connect either directly (jump=None) or by tunneling through an existing
    /// SshSession via direct-tcpip (jump=Some).
    pub async fn connect(
        app: AppHandle,
        vault: Arc<Mutex<Vault>>,
        id: String,
        params: ConnectParams,
        jump: Option<Arc<SshSession>>,
    ) -> AppResult<Self> {
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(7200)),
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 6,
            ..Default::default()
        });

        let forwards: ForwardMap = Arc::new(RwLock::new(HashMap::new()));
        let output_channel: Arc<RwLock<Option<IpcChannel<InvokeResponseBody>>>> =
            Arc::new(RwLock::new(None));
        let handler = ClientHandler::new(
            vault.clone(),
            params.host_id.clone(),
            app.clone(),
            forwards.clone(),
        );

        let mut handle = if let Some(jump_session) = jump {
            let channel = jump_session
                .open_direct_tcpip(&params.host, params.port as u32)
                .await
                .map_err(|e| AppError::Ssh(format!("jump direct-tcpip: {e}")))?;
            let stream = channel.into_stream();
            client::connect_stream(config, stream, handler)
                .await
                .map_err(|e| AppError::Ssh(format!("connect via jump: {e}")))?
        } else {
            let addr = (params.host.as_str(), params.port);
            client::connect(config, addr, handler)
                .await
                .map_err(|e| AppError::Ssh(format!("connect: {e}")))?
        };

        let authenticated = match &params.auth {
            AuthMethod::Password { password } => {
                let pw = Zeroizing::new(password.clone());
                handle
                    .authenticate_password(params.username.clone(), pw.as_str())
                    .await
                    .map_err(|e| AppError::Ssh(format!("auth password: {e}")))?
            }
            AuthMethod::KeyFile { path, passphrase } => {
                let key = auth::load_secret_key(path, passphrase.as_deref())?;
                handle
                    .authenticate_publickey(params.username.clone(), Arc::new(key))
                    .await
                    .map_err(|e| AppError::Ssh(format!("auth key: {e}")))?
            }
        };

        if !authenticated {
            return Err(AppError::Ssh("authentication rejected by server".into()));
        }

        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::Ssh(format!("channel open: {e}")))?;

        // Per-host env vars via SSH SetEnv (RFC 4254 §6.4). Best-effort: many
        // servers reject unrecognized vars via AcceptEnv config; we don't fail.
        for (k, v) in &params.env_vars {
            let _ = channel.set_env(false, k.clone(), v.clone()).await;
        }

        // Match mainstream SSH terminal clients: request an xterm PTY with
        // geometry and let the server apply its normal terminal-mode defaults.
        // Hardcoding mode flags here broke prompt/echo behavior on Ubuntu.
        channel
            .request_pty(
                true,
                "xterm-256color",
                params.cols as u32,
                params.rows as u32,
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| AppError::Ssh(format!("request_pty: {e}")))?;

        channel
            .request_shell(true)
            .await
            .map_err(|e| AppError::Ssh(format!("request_shell: {e}")))?;

        // Startup snippet — sent through the shell after it opens. The user
        // didn't type this; we don't auto-Enter unless the snippet already
        // ends in a newline.
        if let Some(snip) = params.startup_snippet.as_ref() {
            let trimmed = snip.trim_end_matches('\n');
            if !trimmed.is_empty() {
                let payload = format!("{trimmed}\n");
                let _ = channel.data(payload.as_bytes()).await;
            }
        }

        {
            let vault = vault.lock().await;
            vault.record_session_start(&id, &params.host_id)?;
        }

        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<ShellMsg>();
        let session_id_evt = id.clone();
        let app_evt = app.clone();
        let vault_evt = vault.clone();
        let output_channel_evt = output_channel.clone();
        let intentional_close = Arc::new(AtomicBool::new(false));
        let intentional_close_evt = intentional_close.clone();

        // Byte counters live in memory and get flushed to the vault every ~5s.
        // Pre-fix this was a vault.lock() + AES-GCM re-encrypt on EVERY chunk,
        // which froze the UI under bulk output (e.g. agent dumping HTML).
        use std::sync::atomic::{AtomicI64, Ordering};
        let bytes_in_counter = Arc::new(AtomicI64::new(0));
        let bytes_out_counter = Arc::new(AtomicI64::new(0));

        // Periodic flush task — drains the counters into the vault on a cadence.
        // Stops itself when the channel pump drops `flush_alive`.
        let flush_alive = Arc::new(());
        {
            let bytes_in = bytes_in_counter.clone();
            let bytes_out = bytes_out_counter.clone();
            let vault_ref = vault_evt.clone();
            let session_id_ref = session_id_evt.clone();
            let alive = Arc::downgrade(&flush_alive);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    if alive.upgrade().is_none() {
                        break; // session done
                    }
                    let din = bytes_in.swap(0, Ordering::Relaxed);
                    let dout = bytes_out.swap(0, Ordering::Relaxed);
                    if din == 0 && dout == 0 {
                        continue;
                    }
                    // Hold the vault lock only for the fast SQLite write +
                    // snapshot of checkpoint inputs. The slow AES-GCM encrypt
                    // + atomic file rewrite runs in spawn_blocking with the
                    // lock released, so other sessions and Tauri commands
                    // are not blocked while we re-encrypt the snapshot.
                    let checkpoint_inputs = {
                        let vault = vault_ref.lock().await;
                        let _ = vault.record_session_bytes_runtime_only(&session_id_ref, din, dout);
                        vault.prepare_checkpoint()
                    };
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::vault::run_checkpoint_blocking(checkpoint_inputs)
                    })
                    .await;
                }
            });
        }

        // Output batching: coalesce multiple ChannelMsg::Data into one emit.
        // Flushes when the buffer hits FLUSH_BYTES, or after FLUSH_MS since the
        // first byte in the current buffer (whichever comes first). This cuts
        // IPC chatter ~10-50x on bulk output without adding meaningful latency
        // for interactive typing (~8ms is well below human-perceptible jitter).
        // 64 KB matches what most SSH clients (incl. iTerm/Ghostty) use.
        // Frontend writes directly to xterm.js now (no artificial throttle), so
        // bigger batches reduce IPC chatter without hurting interactive latency.
        const FLUSH_BYTES: usize = 64 * 1024;
        const FLUSH_MS: u64 = 4;

        let _flush_alive_owner = flush_alive; // keep alive until task ends

        let pump_handle = tokio::spawn(async move {
            let mut out_buf: Vec<u8> = Vec::with_capacity(FLUSH_BYTES);
            let mut err_buf: Vec<u8> = Vec::with_capacity(FLUSH_BYTES);
            let flush_out =
                |buf: &mut Vec<u8>,
                 app: &AppHandle,
                 sid: &str,
                 channel: &Option<IpcChannel<InvokeResponseBody>>| {
                    if buf.is_empty() {
                        return;
                    }
                    if let Some(channel) = channel {
                        let data = std::mem::replace(buf, Vec::with_capacity(FLUSH_BYTES));
                        let send_result = channel.send(InvokeResponseBody::Raw(data.clone()));
                        if send_result.is_err() {
                            let payload = base64_encode(&data);
                            let _ = app.emit(&format!("ssh://{}/out", sid), payload);
                        }
                    } else {
                        let payload = base64_encode(buf);
                        let _ = app.emit(&format!("ssh://{}/out", sid), payload);
                        buf.clear();
                    }
                };
            let flush_err =
                |buf: &mut Vec<u8>,
                 app: &AppHandle,
                 sid: &str,
                 channel: &Option<IpcChannel<InvokeResponseBody>>| {
                    if buf.is_empty() {
                        return;
                    }
                    if let Some(channel) = channel {
                        let data = std::mem::replace(buf, Vec::with_capacity(FLUSH_BYTES));
                        if channel.send(InvokeResponseBody::Raw(data.clone())).is_err() {
                            let payload = base64_encode(&data);
                            let _ = app.emit(&format!("ssh://{}/err", sid), payload);
                        }
                    } else {
                        let payload = base64_encode(buf);
                        let _ = app.emit(&format!("ssh://{}/err", sid), payload);
                        buf.clear();
                    }
                };
            loop {
                // Sleep duration: if we have buffered data, we want to wake within
                // FLUSH_MS to flush it. If buffers empty, sleep "forever" (1h).
                let buffered = !out_buf.is_empty() || !err_buf.is_empty();
                let sleep_dur = if buffered {
                    std::time::Duration::from_millis(FLUSH_MS)
                } else {
                    std::time::Duration::from_secs(3600)
                };
                tokio::select! {
                    // Input must win whenever the user is typing. Agent UIs like
                    // Claude can produce steady output, and a fair select can keep
                    // picking output reads while Up/Backspace/letters wait behind
                    // the stream. SSH clients should feel input-first.
                    biased;
                    msg = input_rx.recv() => {
                        match msg {
                            Some(ShellMsg::Data(bytes)) => {
                                let sent = tokio::time::timeout(
                                    Duration::from_secs(5),
                                    channel.data(&bytes[..]),
                                ).await;
                                if !matches!(sent, Ok(Ok(_))) {
                                    tracing::warn!("ssh shell data write failed or timed out");
                                    break;
                                }
                                bytes_out_counter.fetch_add(
                                    i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                                    Ordering::Relaxed,
                                );
                            }
                            Some(ShellMsg::Resize { cols, rows, ack }) => {
                                let result = tokio::time::timeout(
                                    Duration::from_millis(250),
                                    channel.window_change(cols, rows, 0, 0),
                                ).await;
                                let ack_result = match result {
                                    Ok(Ok(_)) => Ok(()),
                                    Ok(Err(e)) => {
                                        let message = format!("ssh window_change: {e}");
                                        tracing::warn!(cols, rows, error = %message, "ssh window_change failed; keeping shell pump alive");
                                        Err(message)
                                    }
                                    Err(_) => {
                                        let message = "ssh window_change timed out".to_string();
                                        tracing::warn!(cols, rows, "ssh window_change timed out; keeping shell pump alive");
                                        Err(message)
                                    }
                                };
                                let _ = ack.send(ack_result);
                            }
                            None => break,
                        }
                    }
                    _ = tokio::time::sleep(sleep_dur), if buffered => {
                        let channel = output_channel_evt.read().await.clone();
                        flush_out(&mut out_buf, &app_evt, &session_id_evt, &channel);
                        flush_err(&mut err_buf, &app_evt, &session_id_evt, &channel);
                    }
                    msg = channel.wait() => {
                        let Some(msg) = msg else {
                            crate::commands::diag_write(&format!(
                                "ssh pump channel.wait() returned None — channel closed sid={session_id_evt}"
                            ));
                            break
                        };
                        match msg {
                            ChannelMsg::Data { data } => {
                                bytes_in_counter.fetch_add(
                                    i64::try_from(data.len()).unwrap_or(i64::MAX),
                                    Ordering::Relaxed,
                                );
                                out_buf.extend_from_slice(&data[..]);
                                if out_buf.len() >= FLUSH_BYTES {
                                    let channel = output_channel_evt.read().await.clone();
                                    flush_out(&mut out_buf, &app_evt, &session_id_evt, &channel);
                                }
                            }
                            ChannelMsg::ExtendedData { data, .. } => {
                                bytes_in_counter.fetch_add(
                                    i64::try_from(data.len()).unwrap_or(i64::MAX),
                                    Ordering::Relaxed,
                                );
                                err_buf.extend_from_slice(&data[..]);
                                if err_buf.len() >= FLUSH_BYTES {
                                    let channel = output_channel_evt.read().await.clone();
                                    flush_err(&mut err_buf, &app_evt, &session_id_evt, &channel);
                                }
                            }
                            ChannelMsg::ExitStatus { exit_status } => {
                                let channel = output_channel_evt.read().await.clone();
                                flush_out(&mut out_buf, &app_evt, &session_id_evt, &channel);
                                flush_err(&mut err_buf, &app_evt, &session_id_evt, &channel);
                                let _ = app_evt.emit(&format!("ssh://{}/exit", session_id_evt), exit_status);
                            }
                            ChannelMsg::Eof | ChannelMsg::Close => {
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Final flush + close marker for any straggler bytes.
            let channel = output_channel_evt.read().await.clone();
            flush_out(&mut out_buf, &app_evt, &session_id_evt, &channel);
            flush_err(&mut err_buf, &app_evt, &session_id_evt, &channel);

            // Emit /close for every exit path. Existing listeners (terminal
            // "[session closed]" overlay) still fire as before.
            let _ = app_evt.emit(&format!("ssh://{}/close", session_id_evt), ());
            // Additionally emit /disconnected for UNEXPECTED exits — server
            // dropped us, keepalive failed, channel closed without our doing.
            // The frontend uses this to trigger auto-reconnect with backoff.
            // intentional_close=true means SshSession::disconnect() was called
            // (user closed the tab, app shutting down) — no reconnect needed.
            if !intentional_close_evt.load(Ordering::Acquire) {
                let _ = app_evt.emit(&format!("ssh://{}/disconnected", session_id_evt), ());
            }

            // Final byte-counter flush + mark session ended.
            let din = bytes_in_counter.swap(0, Ordering::Relaxed);
            let dout = bytes_out_counter.swap(0, Ordering::Relaxed);
            let vault = vault_evt.lock().await;
            if din != 0 || dout != 0 {
                let _ = vault.record_session_bytes(&session_id_evt, din, dout);
            }
            let _ = vault.record_session_end(&session_id_evt);
            drop(_flush_alive_owner);
        });

        Ok(Self {
            id,
            handle: Arc::new(Mutex::new(handle)),
            input_tx,
            forwards,
            output_channel,
            cached_agent: Arc::new(RwLock::new(None)),
            cached_sftp: Arc::new(Mutex::new(None)),
            sftp_active_ops: Arc::new(AtomicUsize::new(0)),
            sftp_ops_idle: Arc::new(Notify::new()),
            sftp_lifecycle: Arc::new(Mutex::new(())),
            intentional_close,
            pump_handle: Arc::new(Mutex::new(Some(pump_handle))),
        })
    }

    /// Open a dedicated SSH transport for background SFTP work. This keeps large
    /// uploads/downloads from competing with the interactive shell channel on the
    /// user's visible terminal session.
    pub async fn connect_sftp_only(
        app: AppHandle,
        vault: Arc<Mutex<Vault>>,
        params: ConnectParams,
        jump: Option<Arc<SshSession>>,
    ) -> AppResult<SftpOnlySession> {
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(7200)),
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 6,
            ..Default::default()
        });
        let forwards: ForwardMap = Arc::new(RwLock::new(HashMap::new()));
        let handler = ClientHandler::new(vault, params.host_id.clone(), app, forwards);
        let mut handle = if let Some(jump_session) = jump {
            let channel = jump_session
                .open_direct_tcpip(&params.host, params.port as u32)
                .await
                .map_err(|e| AppError::Ssh(format!("transfer jump direct-tcpip: {e}")))?;
            client::connect_stream(config, channel.into_stream(), handler)
                .await
                .map_err(|e| AppError::Ssh(format!("transfer connect via jump: {e}")))?
        } else {
            client::connect(config, (params.host.as_str(), params.port), handler)
                .await
                .map_err(|e| AppError::Ssh(format!("transfer connect: {e}")))?
        };
        let authenticated = match &params.auth {
            AuthMethod::Password { password } => {
                let pw = Zeroizing::new(password.clone());
                handle
                    .authenticate_password(params.username.clone(), pw.as_str())
                    .await
                    .map_err(|e| AppError::Ssh(format!("transfer auth password: {e}")))?
            }
            AuthMethod::KeyFile { path, passphrase } => {
                let key = auth::load_secret_key(path, passphrase.as_deref())?;
                handle
                    .authenticate_publickey(params.username.clone(), Arc::new(key))
                    .await
                    .map_err(|e| AppError::Ssh(format!("transfer auth key: {e}")))?
            }
        };
        if !authenticated {
            return Err(AppError::Ssh(
                "transfer authentication rejected by server".into(),
            ));
        }
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::Ssh(format!("transfer sftp channel open: {e}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| AppError::Ssh(format!("transfer sftp subsystem: {e}")))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| AppError::Sftp(format!("transfer sftp init: {e}")))?;
        Ok(SftpOnlySession { handle, sftp })
    }

    pub async fn bind_output_channel(&self, channel: IpcChannel<InvokeResponseBody>) {
        *self.output_channel.write().await = Some(channel);
    }

    /// Returns the cached (kind, pid) if known. Only POSITIVE detections are
    /// cached — if no agent is currently running we re-probe on the next call.
    /// This matters because agents come and go (user might start `claude`
    /// after first connecting), and a stale `None` would mean every drop is
    /// formatted as "no agent" forever.
    pub async fn cached_or_detect_agent(
        &self,
    ) -> AppResult<Option<(crate::agent_detect::AgentKind, u32)>> {
        if let Some(Some((kind, pid))) = *self.cached_agent.read().await {
            // Revalidate before trusting the cached PID: the agent may have
            // exited and its PID been recycled by an unrelated process, which
            // would make us resolve the wrong process's cwd. Re-probe on miss.
            if crate::agent_detect::pid_matches(self, kind, pid).await {
                return Ok(Some((kind, pid)));
            }
            *self.cached_agent.write().await = None;
            tracing::debug!(pid, "cached agent PID stale; re-detecting");
        }
        let detected = crate::agent_detect::detect(self).await?;
        if detected.is_some() {
            *self.cached_agent.write().await = Some(detected);
            tracing::info!(?detected, "agent detected and cached for session");
        }
        Ok(detected)
    }

    /// Force a re-detection (e.g., user switched agents mid-session).
    pub async fn invalidate_agent_cache(&self) {
        *self.cached_agent.write().await = None;
    }

    /// Open a direct-tcpip channel for local-forward and SOCKS5 paths.
    pub async fn open_direct_tcpip(&self, host: &str, port: u32) -> AppResult<Channel<Msg>> {
        let handle = self.handle.lock().await;
        handle
            .channel_open_direct_tcpip(host, port, "127.0.0.1", 0)
            .await
            .map_err(|e| AppError::Ssh(format!("direct-tcpip {host}:{port}: {e}")))
    }

    /// Ask the server to listen on bind_host:bind_port and forward incoming
    /// connections back to us. Records the local destination for the handler.
    pub async fn register_remote_forward(
        &self,
        remote_bind_host: String,
        remote_bind_port: u32,
        local_host: String,
        local_port: u16,
    ) -> AppResult<u32> {
        let mut handle = self.handle.lock().await;
        let bound_port = handle
            .tcpip_forward(&remote_bind_host, remote_bind_port)
            .await
            .map_err(|e| AppError::Ssh(format!("tcpip_forward: {e}")))?;
        drop(handle);
        let effective_remote_port = if remote_bind_port == 0 {
            bound_port
        } else {
            remote_bind_port
        };
        self.forwards.write().await.insert(
            (remote_bind_host, effective_remote_port),
            (local_host, local_port),
        );
        Ok(effective_remote_port)
    }

    pub async fn cancel_remote_forward(
        &self,
        remote_bind_host: &str,
        remote_bind_port: u32,
    ) -> AppResult<()> {
        let handle = self.handle.lock().await;
        let _ = handle
            .cancel_tcpip_forward(remote_bind_host, remote_bind_port)
            .await;
        drop(handle);
        self.forwards
            .write()
            .await
            .remove(&(remote_bind_host.to_string(), remote_bind_port));
        Ok(())
    }

    pub fn send(&self, bytes: Vec<u8>) -> AppResult<()> {
        self.input_tx
            .send(ShellMsg::Data(bytes))
            .map_err(|_| AppError::Ssh("session channel closed".into()))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> AppResult<()> {
        let (ack, ack_rx) = oneshot::channel();
        self.input_tx
            .send(ShellMsg::Resize {
                cols: cols as u32,
                rows: rows as u32,
                ack,
            })
            .map_err(|_| AppError::Ssh("session channel closed".into()))?;
        match tokio::time::timeout(Duration::from_millis(350), ack_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(message))) => Err(AppError::Ssh(message)),
            Ok(Err(_)) => Err(AppError::Ssh("session channel closed".into())),
            Err(_) => Err(AppError::Ssh("resize acknowledgement timed out".into())),
        }
    }

    pub async fn disconnect(&self) -> AppResult<()> {
        // Mark BEFORE sending the SSH disconnect so the pump's exit-path emits
        // /close (intentional) and NOT /disconnected (unexpected) — otherwise
        // the frontend would spuriously auto-reconnect to a session the user
        // just closed.
        self.intentional_close.store(true, Ordering::Release);
        {
            let handle = self.handle.lock().await;
            // Best-effort: if the connection is already half-dead this can fail.
            let _ = handle
                .disconnect(Disconnect::ByApplication, "user disconnect", "")
                .await;
        }
        // Give the pump a short window to drain naturally (final flush, vault
        // bookkeeping), then abort if it's wedged. Without the abort a hung
        // channel.wait() would leak the task indefinitely.
        let handle = { self.pump_handle.lock().await.take() };
        if let Some(h) = handle {
            let aborter = h.abort_handle();
            match tokio::time::timeout(Duration::from_millis(500), h).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) if e.is_cancelled() => {}
                Ok(Err(e)) => {
                    tracing::warn!("pump task join error on disconnect: {e}");
                }
                Err(_) => {
                    aborter.abort();
                    tracing::warn!("pump task did not exit within 500ms; aborted");
                }
            }
        }
        Ok(())
    }

    /// Open a fresh SFTP session over a NEW channel on the same connection.
    async fn open_sftp_fresh(&self) -> AppResult<SftpSession> {
        let handle = self.handle.lock().await;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::Ssh(format!("sftp channel open: {e}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| AppError::Ssh(format!("sftp subsystem: {e}")))?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| AppError::Sftp(format!("sftp init: {e}")))?;
        Ok(sftp)
    }

    /// Reuse the session's SFTP subsystem. The mutex intentionally guards the
    /// open path so rapid folder clicks join the same open instead of racing
    /// multiple channel negotiations.
    pub async fn sftp(&self) -> AppResult<Arc<SftpSession>> {
        let mut cached = self.cached_sftp.lock().await;
        if let Some(sftp) = cached.as_ref() {
            return Ok(sftp.clone());
        }
        let sftp = match self.open_sftp_fresh().await {
            Ok(sftp) => sftp,
            Err(first_err) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.open_sftp_fresh().await.map_err(|second_err| {
                    AppError::Ssh(format!(
                        "{first_err}; retry after reopening sftp failed: {second_err}"
                    ))
                })?
            }
        };
        let sftp = Arc::new(sftp);
        *cached = Some(sftp.clone());
        Ok(sftp)
    }

    /// Acquire the SFTP session AND bump the active-ops refcount. The returned
    /// `SftpGuard` decrements on drop, and `invalidate_sftp` waits for the
    /// count to hit 0 before tearing the session down. Use this around any
    /// SFTP operation (list, upload, download, rename, mkdir, remove, chmod).
    pub async fn acquire_sftp(&self) -> AppResult<SftpGuard> {
        let _lifecycle = self.sftp_lifecycle.lock().await;
        self.sftp_active_ops.fetch_add(1, Ordering::AcqRel);
        let sftp = match self.sftp().await {
            Ok(sftp) => sftp,
            Err(e) => {
                if self.sftp_active_ops.fetch_sub(1, Ordering::AcqRel) == 1 {
                    self.sftp_ops_idle.notify_waiters();
                }
                return Err(e);
            }
        };
        Ok(SftpGuard {
            sftp,
            active: self.sftp_active_ops.clone(),
            idle: self.sftp_ops_idle.clone(),
        })
    }

    /// Drop the cached SFTP session, draining in-flight ops first.
    ///
    /// Pre-fix this immediately called `sftp.close().await`, which left
    /// concurrent uploads/lists holding an `Arc<SftpSession>` that now pointed
    /// at a closed subsystem — pending `SSH_FXP_*` packets never got a reply
    /// and hung forever. We now wait up to 2s for the refcount to drain.
    pub async fn invalidate_sftp(&self) {
        let _lifecycle = self.sftp_lifecycle.lock().await;
        const DRAIN_TIMEOUT: Duration = Duration::from_millis(2000);
        let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
        while self.sftp_active_ops.load(Ordering::Acquire) > 0 {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    active = self.sftp_active_ops.load(Ordering::Relaxed),
                    "invalidate_sftp: drain timeout — closing with ops still in-flight"
                );
                break;
            }
            // notify_one wakes us; if no one notifies we still re-check on timeout.
            let _ = tokio::time::timeout(remaining, self.sftp_ops_idle.notified()).await;
        }
        let mut cached = self.cached_sftp.lock().await;
        if let Some(sftp) = cached.take() {
            let _ = sftp.close().await;
        }
    }

    /// One-shot remote command on a new channel; returns stdout bytes (capped).
    ///
    /// The `handle` lock is held ONLY long enough to open the channel — the
    /// channel itself is independent and can run while other operations (SFTP
    /// open, tunnel open, disconnect) acquire the handle. Without this, a slow
    /// `cat /etc/os-release` blocks the entire SSH handle for seconds,
    /// freezing SFTP and any other concurrent operation.
    ///
    /// `EXEC_TIMEOUT` bounds the whole call — a hung server can't wedge the
    /// background OS/agent detection indefinitely.
    pub async fn exec_oneshot(&self, cmd: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
        const EXEC_TIMEOUT: Duration = Duration::from_secs(10);
        let mut channel = {
            let handle = self.handle.lock().await;
            handle
                .channel_open_session()
                .await
                .map_err(|e| AppError::Ssh(format!("exec channel open: {e}")))?
            // handle lock drops here — channel survives independently
        };
        let work = async move {
            channel
                .exec(true, cmd)
                .await
                .map_err(|e| AppError::Ssh(format!("exec: {e}")))?;
            let mut out = Vec::new();
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        out.extend_from_slice(&data[..]);
                        if out.len() >= max_bytes {
                            out.truncate(max_bytes);
                            break;
                        }
                    }
                    ChannelMsg::ExitStatus { .. } => {}
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            // channel drops here on success; russh closes it.
            Ok::<_, AppError>(out)
        };
        tokio::time::timeout(EXEC_TIMEOUT, work)
            .await
            .map_err(|_| AppError::Ssh(format!("exec '{cmd}' timed out after {EXEC_TIMEOUT:?}")))?
        // On timeout, the future is dropped, dropping the channel, which russh closes.
    }
}

/// RAII guard around an SFTP session use. Decrements the per-session
/// active-ops counter on drop and notifies any pending `invalidate_sftp`.
pub struct SftpGuard {
    sftp: Arc<SftpSession>,
    active: Arc<AtomicUsize>,
    idle: Arc<Notify>,
}

impl SftpGuard {
    pub fn sftp(&self) -> &SftpSession {
        &self.sftp
    }
}

impl Drop for SftpGuard {
    fn drop(&mut self) {
        let prev = self.active.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            self.idle.notify_waiters();
        }
    }
}

/// SSH host-key callback enforcing TOFU + fingerprint pinning.
///
/// First connect: record the server's SHA256 fingerprint into the vault and
/// allow. Subsequent connects: match against the stored fingerprint(s). If the
/// fingerprint changes, emit `ssh://host-key-changed/<host_id>` to the renderer
/// and reject. Host-key rotation must be implemented with a backend-owned
/// native confirmation flow; the renderer cannot be allowed to overwrite pins.
pub struct ClientHandler {
    vault: Arc<Mutex<Vault>>,
    host_id: String,
    app: AppHandle,
    forwards: ForwardMap,
}

impl ClientHandler {
    pub fn new(
        vault: Arc<Mutex<Vault>>,
        host_id: String,
        app: AppHandle,
        forwards: ForwardMap,
    ) -> Self {
        Self {
            vault,
            host_id,
            app,
            forwards,
        }
    }
}

/// OpenSSH-style fingerprint: `SHA256:<unpadded-base64>`.
pub fn server_key_fingerprint(key: &PublicKey) -> String {
    let raw = key.fingerprint();
    let trimmed = raw.trim_end_matches('=');
    if trimmed.starts_with("SHA256:") {
        trimmed.to_string()
    } else {
        format!("SHA256:{trimmed}")
    }
}

#[async_trait]
impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let presented = server_key_fingerprint(server_public_key);

        let vault = self.vault.lock().await;
        let known = vault.known_fingerprints_for(&self.host_id).map_err(|e| {
            tracing::error!("known_fingerprints_for: {e}");
            russh::Error::HUP
        })?;

        if known.is_empty() {
            // TOFU: first time we see this host. Record and allow.
            if let Err(e) = vault.record_known_host(&self.host_id, &presented) {
                tracing::error!("record_known_host: {e}");
                return Err(russh::Error::HUP);
            }
            tracing::info!(host_id=%self.host_id, fp=%presented, "host-key first-use recorded");
            let _ = self.app.emit(
                "ssh://host-key-first-seen",
                serde_json::json!({ "host_id": self.host_id, "fingerprint": presented }),
            );
            return Ok(true);
        }

        if known.iter().any(|fp| fp == &presented) {
            return Ok(true);
        }

        // Mismatch — possible MitM or legitimate server-key rotation.
        let payload = serde_json::json!({
            "host_id": self.host_id,
            "presented": presented,
            "known": known,
        });
        let _ = self.app.emit("ssh://host-key-changed", payload);
        tracing::warn!(
            host_id=%self.host_id,
            presented=%presented,
            known=?known,
            "SSH server host-key mismatch — connection refused unless user explicitly accepts"
        );

        Ok(false)
    }

    /// Server pushed us a forwarded-tcpip channel (the back half of a remote
    /// port-forward). Route it to the registered local destination.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        let key = (connected_address.to_string(), connected_port);
        let target = self.forwards.read().await.get(&key).cloned();
        let Some((local_host, local_port)) = target else {
            tracing::warn!(
                "forwarded-tcpip for unregistered bind {connected_address}:{connected_port} — dropping"
            );
            let _ = channel.close().await;
            return Ok(());
        };
        tokio::spawn(async move {
            let mut remote = channel.into_stream();
            match TcpStream::connect((local_host.as_str(), local_port)).await {
                Ok(mut local) => {
                    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                    let _ = local.shutdown().await;
                }
                Err(e) => {
                    tracing::warn!("remote-forward local dial {local_host}:{local_port}: {e}");
                    let _ = remote.shutdown().await;
                }
            }
        });
        Ok(())
    }
}

// Suppress unused-import warning for decode_secret_key (used by auth submodule via re-export check).
#[allow(dead_code)]
fn _unused() {
    let _ = decode_secret_key("", None);
}

/// Minimal base64 encoder. The renderer decodes via atob().
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 << 4) & 0x30) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 << 2) & 0x3c) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
