use crate::agent_detect::{self, AgentKind};
use crate::errors::{AppError, AppResult};
use crate::local_terminal::LocalTerminalSession;
use crate::sftp;
use crate::ssh::{ConnectParams, SftpOnlySession, SshSession};
use crate::vault::{
    AddHostInput, AddKeyInput, AddSnippetInput, AddTunnelInput, HostRow, KeyRow, KnownHostRow,
    SessionLogRow, SnippetRow, TunnelRow,
};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::ipc::{Channel as IpcChannel, InvokeBody, InvokeResponseBody, Request};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn list_hosts(state: State<'_, AppState>) -> AppResult<Vec<HostRow>> {
    let vault = state.vault.lock().await;
    vault.list_hosts()
}

#[tauri::command]
pub async fn add_host(state: State<'_, AppState>, input: AddHostInput) -> AppResult<String> {
    let input = normalize_host_input(input);
    validate_host_auth(&input)?;
    validate_env_json(input.env_json.as_deref())?;
    validate_startup_snippet(input.startup_snippet.as_deref())?;
    let vault = state.vault.lock().await;
    vault.add_host(input)
}

#[tauri::command]
pub async fn delete_host(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let sessions = state.sessions.list_for_host(&id).await;
    for session in sessions {
        state.tunnels.stop_all_for_session(&session.id).await;
        if let Some(registered) = state.sessions.remove(&session.id).await {
            if let Err(e) = registered.disconnect().await {
                tracing::warn!(session_id = %session.id, host_id = %id, "failed to disconnect deleted host session: {e}");
            }
        }
    }
    {
        let vault = state.vault.lock().await;
        vault.delete_host(&id)?;
    }
    let legacy_hostpass_service = legacy_hostpass_service();
    for service in [HOSTPASS_SERVICE, legacy_hostpass_service.as_str()] {
        if let Ok(entry) = keyring::Entry::new(service, &id) {
            let _ = entry.delete_credential();
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn update_host(
    state: State<'_, AppState>,
    id: String,
    input: AddHostInput,
) -> AppResult<()> {
    let input = normalize_host_input(input);
    validate_host_auth(&input)?;
    validate_env_json(input.env_json.as_deref())?;
    validate_startup_snippet(input.startup_snippet.as_deref())?;
    let vault = state.vault.lock().await;
    vault.update_host(&id, input)
}

fn normalize_host_input(mut input: AddHostInput) -> AddHostInput {
    input.hostname = input.hostname.trim().to_string();
    input.username = input.username.trim().to_string();
    input.label = input.label.trim().to_string();
    if input.label.is_empty() {
        input.label = input.hostname.clone();
    }
    input
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub host_id: String,
    pub auth_secret: Option<String>, // password or key passphrase from the renderer
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub remember_key_passphrase: bool,
}

#[derive(Serialize)]
pub struct ConnectResponse {
    pub session_id: String,
}

#[derive(Serialize)]
pub struct RemoteFilePreview {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[tauri::command]
pub async fn start_local_terminal(
    app: AppHandle,
    state: State<'_, AppState>,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) -> AppResult<ConnectResponse> {
    validate_pty_size(cols, rows)?;
    let cwd = resolve_local_terminal_cwd(cwd)?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let session = LocalTerminalSession::start(app, session_id.clone(), cols, rows, cwd)?;
    let registered_id = state.local_terminals.insert(session).await;
    Ok(ConnectResponse {
        session_id: registered_id,
    })
}

fn resolve_local_terminal_cwd(cwd: Option<String>) -> AppResult<Option<std::path::PathBuf>> {
    let Some(raw) = cwd else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let path = std::path::Path::new(trimmed);
    if !path.is_absolute() {
        return Err(AppError::Invalid(
            "local terminal folder must be absolute".into(),
        ));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| AppError::Invalid(format!("local terminal folder unavailable: {e}")))?;
    let meta = std::fs::metadata(&canonical)
        .map_err(|e| AppError::Invalid(format!("local terminal folder unavailable: {e}")))?;
    if !meta.is_dir() {
        return Err(AppError::Invalid(
            "local terminal folder must be a directory".into(),
        ));
    }
    Ok(Some(canonical))
}

#[tauri::command]
pub async fn bind_terminal_output(
    state: State<'_, AppState>,
    session_id: String,
    channel: IpcChannel<InvokeResponseBody>,
) -> AppResult<()> {
    if let Ok(session) = state.sessions.get(&session_id).await {
        session.bind_output_channel(channel).await;
        return Ok(());
    }
    if let Some(session) = state.local_terminals.get(&session_id).await {
        session.bind_output_channel(channel);
        return Ok(());
    }
    Err(AppError::SessionNotFound(session_id))
}

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    req: ConnectRequest,
) -> AppResult<ConnectResponse> {
    validate_pty_size(req.cols, req.rows)?;
    // Pull host config from vault.
    let host = {
        let vault = state.vault.lock().await;
        vault
            .list_hosts()?
            .into_iter()
            .find(|h| h.id == req.host_id)
            .ok_or_else(|| AppError::Invalid(format!("host_id not found: {}", req.host_id)))?
    };

    let key_id_for_passphrase = if host.auth_kind == "key_file" {
        match host.key_path.as_deref() {
            Some(path) => key_id_for_private_path(&state, path).await.ok(),
            None => None,
        }
    } else {
        None
    };
    let provided_key_passphrase = req.auth_secret.clone().filter(|s| !s.is_empty());
    let keychain_passphrase = if provided_key_passphrase.is_none() {
        key_id_for_passphrase
            .as_deref()
            .and_then(|key_id| get_key_passphrase(key_id).ok())
    } else {
        None
    };

    let provided_password = req.auth_secret.clone().filter(|s| !s.is_empty());
    let stored_password = if host.auth_kind == "password" && provided_password.is_none() {
        get_host_password(&state, &host.id).await.ok().flatten()
    } else {
        None
    };

    let auth = match host.auth_kind.as_str() {
        "password" => crate::ssh::AuthMethod::Password {
            password: provided_password
                .clone()
                .or(stored_password)
                .ok_or_else(|| AppError::Invalid("password required".into()))?,
        },
        "key_file" => crate::ssh::AuthMethod::KeyFile {
            path: host
                .key_path
                .clone()
                .ok_or_else(|| AppError::Invalid("key_path missing".into()))?,
            passphrase: provided_key_passphrase.clone().or(keychain_passphrase),
        },
        other => return Err(AppError::Invalid(format!("unknown auth kind: {other}"))),
    };

    // Resolve ProxyJump: if host.jump_host_id is set, look up an active session
    // for that host. Multi-hop chains are supported via recursive lookup — user
    // is expected to have already connected to each jump in the chain.
    let jump = if let Some(jump_id) = host.jump_host_id.as_deref() {
        let active = find_active_session_for_host(&state, jump_id).await?;
        Some(active)
    } else {
        None
    };

    validate_env_json(host.env_json.as_deref())?;
    let env_vars = parse_env_json(host.env_json.as_deref());
    validate_env_vars(&env_vars)?;
    let startup_snippet = host
        .startup_snippet
        .clone()
        .filter(|s| !s.trim().is_empty());
    validate_startup_snippet(startup_snippet.as_deref())?;

    let params = ConnectParams {
        host: host.hostname,
        port: host.port as u16,
        username: host.username,
        auth,
        cols: req.cols,
        rows: req.rows,
        host_id: host.id.clone(),
        env_vars,
        startup_snippet,
    };

    // Generate the session id up-front so we can stream output before insert.
    let session_id = uuid::Uuid::new_v4().to_string();
    let session =
        SshSession::connect(app, state.vault.clone(), session_id.clone(), params, jump).await?;
    if req.remember_key_passphrase {
        if let (Some(key_id), Some(passphrase)) = (key_id_for_passphrase, provided_key_passphrase) {
            if let Err(e) = set_key_passphrase(key_id.clone(), passphrase.clone()).await {
                tracing::warn!(key_id = %key_id, "connected, but failed to remember key passphrase: {e}");
            }
        }
        if host.auth_kind == "password" {
            if let Some(password) = provided_password {
                if let Err(e) = set_host_password(state.clone(), host.id.clone(), password).await {
                    tracing::warn!(host_id = %host.id, "connected, but failed to remember host password: {e}");
                }
            }
        }
    }

    let registered_id = state.sessions.insert(host.id.clone(), session).await;
    Ok(ConnectResponse {
        session_id: registered_id,
    })
}

async fn transfer_connect_params_for_host(
    state: &State<'_, AppState>,
    host_id: &str,
) -> AppResult<(ConnectParams, Option<std::sync::Arc<SshSession>>)> {
    let host = {
        let vault = state.vault.lock().await;
        vault
            .list_hosts()?
            .into_iter()
            .find(|h| h.id == host_id)
            .ok_or_else(|| AppError::Invalid(format!("host_id not found: {host_id}")))?
    };

    let auth = match host.auth_kind.as_str() {
        "password" => crate::ssh::AuthMethod::Password {
            password: get_host_password(state, &host.id).await?.ok_or_else(|| {
                AppError::Invalid("saved host password required for background upload".into())
            })?,
        },
        "key_file" => {
            let path = host
                .key_path
                .clone()
                .ok_or_else(|| AppError::Invalid("key_path missing".into()))?;
            let passphrase = key_id_for_private_path(state, &path)
                .await
                .ok()
                .and_then(|key_id| get_key_passphrase(&key_id).ok());
            crate::ssh::AuthMethod::KeyFile { path, passphrase }
        }
        other => return Err(AppError::Invalid(format!("unknown auth kind: {other}"))),
    };

    let jump = if let Some(jump_id) = host.jump_host_id.as_deref() {
        Some(find_active_session_for_host(state, jump_id).await?)
    } else {
        None
    };

    Ok((
        ConnectParams {
            host: host.hostname,
            port: host.port as u16,
            username: host.username,
            auth,
            cols: 80,
            rows: 24,
            host_id: host.id,
            env_vars: Vec::new(),
            startup_snippet: None,
        },
        jump,
    ))
}

async fn connect_transfer_sftp_for_session(
    app: AppHandle,
    state: &State<'_, AppState>,
    session_id: &str,
) -> AppResult<SftpOnlySession> {
    let host_id = state.sessions.host_id_for_session(session_id).await?;
    let (params, jump) = transfer_connect_params_for_host(state, &host_id).await?;
    SshSession::connect_sftp_only(app, state.vault.clone(), params, jump).await
}

fn get_key_passphrase(key_id: &str) -> AppResult<String> {
    let legacy_service = legacy_keypass_service();
    keychain_get_with_migration(KEYPASS_SERVICE, &legacy_service, key_id)
}

async fn key_id_for_private_path(
    state: &State<'_, AppState>,
    private_path: &str,
) -> AppResult<String> {
    let vault = state.vault.lock().await;
    vault
        .list_keys()?
        .into_iter()
        .find(|k| k.private_path.as_deref() == Some(private_path))
        .map(|k| k.id)
        .ok_or_else(|| AppError::Invalid("key not found in keychain".into()))
}

async fn find_active_session_for_host(
    state: &State<'_, AppState>,
    host_id: &str,
) -> AppResult<std::sync::Arc<SshSession>> {
    let sessions = state.sessions.list_for_host(host_id).await;
    sessions.into_iter().next().ok_or_else(|| {
        AppError::Invalid(format!(
            "jump host {host_id} has no active session — connect to it first"
        ))
    })
}

fn parse_env_json(s: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = s else { return Vec::new() };
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw)
        .map(|m| m.into_iter().collect())
        .unwrap_or_default()
}

fn validate_host_auth(input: &AddHostInput) -> AppResult<()> {
    match input.auth_kind.as_str() {
        "password" => Ok(()),
        "key_file" => {
            if input
                .key_path
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                Err(AppError::Invalid(
                    "key_path required for key_file auth".into(),
                ))
            } else {
                Ok(())
            }
        }
        other => Err(AppError::Invalid(format!("unknown auth kind: {other}"))),
    }
}

fn validate_env_json(s: Option<&str>) -> AppResult<()> {
    let Some(raw) = s.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if raw.len() > MAX_ENV_JSON_BYTES {
        return Err(AppError::Invalid(format!(
            "env_json is too large ({} bytes, max {} bytes)",
            raw.len(),
            MAX_ENV_JSON_BYTES
        )));
    }
    let vars =
        serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw).map_err(|e| {
            AppError::Invalid(format!("env_json must be an object of string values: {e}"))
        })?;
    let vars: Vec<(String, String)> = vars.into_iter().collect();
    validate_env_vars(&vars)
}

const MAX_ENV_JSON_BYTES: usize = 64 * 1024;
const MAX_ENV_VARS: usize = 128;
const MAX_ENV_KEY_CHARS: usize = 128;
const MAX_ENV_VALUE_BYTES: usize = 4096;
const MAX_STARTUP_SNIPPET_BYTES: usize = 128 * 1024;

fn validate_env_vars(vars: &[(String, String)]) -> AppResult<()> {
    if vars.len() > MAX_ENV_VARS {
        return Err(AppError::Invalid(format!(
            "too many environment variables ({} max)",
            MAX_ENV_VARS
        )));
    }
    for (key, value) in vars {
        if key.is_empty() || key.chars().count() > MAX_ENV_KEY_CHARS {
            return Err(AppError::Invalid(format!(
                "environment variable names must be 1-{MAX_ENV_KEY_CHARS} characters"
            )));
        }
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(AppError::Invalid(
                "environment variable names may only contain letters, numbers, and underscores"
                    .into(),
            ));
        }
        if key.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Err(AppError::Invalid(
                "environment variable names cannot start with a number".into(),
            ));
        }
        if value.len() > MAX_ENV_VALUE_BYTES {
            return Err(AppError::Invalid(format!(
                "environment variable {key} is too large (max {MAX_ENV_VALUE_BYTES} bytes)"
            )));
        }
    }
    Ok(())
}

fn validate_startup_snippet(s: Option<&str>) -> AppResult<()> {
    let Some(raw) = s else { return Ok(()) };
    if raw.len() > MAX_STARTUP_SNIPPET_BYTES {
        return Err(AppError::Invalid(format!(
            "startup snippet is too large ({} bytes, max {} bytes)",
            raw.len(),
            MAX_STARTUP_SNIPPET_BYTES
        )));
    }
    validate_terminal_input_len(raw.len())
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.tunnels.stop_all_for_session(&session_id).await;
    if let Some(session) = state.sessions.remove(&session_id).await {
        session.disconnect().await?;
        return Ok(());
    }
    if let Some(session) = state.local_terminals.remove(&session_id).await {
        session.disconnect()?;
    }
    Ok(())
}

// ── PORT FORWARDING ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StartTunnelRequest {
    pub tunnel_id: String,
    pub session_id: String,
}

#[tauri::command]
pub async fn start_tunnel(state: State<'_, AppState>, req: StartTunnelRequest) -> AppResult<()> {
    let tunnel = {
        let vault = state.vault.lock().await;
        vault
            .list_tunnels()?
            .into_iter()
            .find(|t| t.id == req.tunnel_id)
            .ok_or_else(|| AppError::Invalid(format!("tunnel not found: {}", req.tunnel_id)))?
    };
    validate_tunnel_parts(
        &tunnel.label,
        &tunnel.kind,
        tunnel.local_port,
        tunnel.remote_host.as_deref(),
        tunnel.remote_port,
    )?;
    let session = state.sessions.get(&req.session_id).await?;
    let local_port = u16::try_from(tunnel.local_port)
        .map_err(|_| AppError::Invalid("local_port out of u16 range".into()))?;
    match tunnel.kind.as_str() {
        "local" => {
            let rhost = tunnel
                .remote_host
                .clone()
                .unwrap_or_else(|| "127.0.0.1".into());
            let rport = u16::try_from(tunnel.remote_port.unwrap_or(0))
                .map_err(|_| AppError::Invalid("remote_port out of u16 range".into()))?;
            state
                .tunnels
                .start_local(tunnel.id, session, local_port, rhost, rport)
                .await
        }
        "dynamic" => {
            state
                .tunnels
                .start_dynamic(tunnel.id, session, local_port)
                .await
        }
        "remote" => {
            // local_port is the REMOTE bind port for this kind; remote_host/port
            // describe the local destination the remote should reach back to.
            let lhost = tunnel
                .remote_host
                .clone()
                .unwrap_or_else(|| "127.0.0.1".into());
            let lport = u16::try_from(tunnel.remote_port.unwrap_or(0))
                .map_err(|_| AppError::Invalid("local destination port out of u16 range".into()))?;
            state
                .tunnels
                .start_remote(
                    tunnel.id,
                    session,
                    "127.0.0.1".to_string(),
                    local_port,
                    lhost,
                    lport,
                )
                .await
        }
        other => Err(AppError::Invalid(format!("unknown tunnel kind: {other}"))),
    }
}

#[tauri::command]
pub async fn stop_tunnel(state: State<'_, AppState>, tunnel_id: String) -> AppResult<()> {
    state.tunnels.stop(&tunnel_id, &state.sessions).await
}

#[tauri::command]
pub async fn active_tunnels(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    Ok(state.tunnels.active_ids().await)
}

#[tauri::command]
pub async fn send_input(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> AppResult<()> {
    validate_terminal_input_len(data.len())?;
    send_to_terminal_session(&state, &session_id, data.into_bytes()).await
}

#[tauri::command]
pub async fn send_input_raw(state: State<'_, AppState>, request: Request<'_>) -> AppResult<()> {
    let session_id = request
        .headers()
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Invalid("missing x-session-id".into()))?;
    let bytes = match request.body() {
        InvokeBody::Raw(bytes) => bytes.clone(),
        InvokeBody::Json(value) => {
            if let Some(s) = value.as_str() {
                s.as_bytes().to_vec()
            } else {
                return Err(AppError::Invalid("raw input payload required".into()));
            }
        }
    };
    if bytes.is_empty() {
        return Ok(());
    }
    validate_terminal_input_len(bytes.len())?;
    send_to_terminal_session(&state, session_id, bytes).await
}

#[tauri::command]
pub async fn copy_local_image_to_clipboard(path: String) -> AppResult<()> {
    copy_local_image_to_clipboard_impl(&path).await
}

async fn copy_local_image_to_clipboard_impl(path: &str) -> AppResult<()> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(AppError::Invalid("image path must be absolute".into()));
    }
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| AppError::Invalid(format!("image unavailable: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Invalid("image path must be a file".into()));
    }
    if meta.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(AppError::Invalid(format!(
            "image is too large ({} bytes, max {} bytes)",
            meta.len(),
            MAX_CLIPBOARD_IMAGE_BYTES
        )));
    }
    let image_class = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
    {
        Some(ext) if ext == "png" => {
            let left = char::from_u32(0x00ab).unwrap_or('<');
            let right = char::from_u32(0x00bb).unwrap_or('>');
            format!("{left}class PNGf{right}")
        }
        Some(ext) if ext == "jpg" || ext == "jpeg" => "JPEG picture".to_string(),
        Some(ext) if ext == "gif" => "GIF picture".to_string(),
        Some(ext) if ext == "tif" || ext == "tiff" => "TIFF picture".to_string(),
        _ => return Err(AppError::Invalid("unsupported image type".into())),
    };
    let script = format!(
        "on run argv\nset the clipboard to (read (POSIX file (item 1 of argv)) as {image_class})\nend run"
    );
    let path_arg = path.to_string_lossy().to_string();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .arg(path_arg)
            .output()
    })
    .await
    .map_err(|e| AppError::Ssh(format!("clipboard task failed: {e}")))?
    .map_err(|e| AppError::Ssh(format!("clipboard command failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::Ssh(format!(
            "copy image to clipboard failed: {stderr}"
        )));
    }
    Ok(())
}

async fn send_to_terminal_session(
    state: &State<'_, AppState>,
    session_id: &str,
    bytes: Vec<u8>,
) -> AppResult<()> {
    if let Ok(session) = state.sessions.get(session_id).await {
        return session.send(bytes);
    }
    if let Some(session) = state.local_terminals.get(session_id).await {
        return session.send(bytes);
    }
    Err(AppError::SessionNotFound(session_id.to_string()))
}

/// Best-effort append to a private per-user diagnostic log. Never panics,
/// never logs an error on its own failure — diagnostic-only, must not affect
/// runtime.
pub fn diag_write(line: &str) {
    use std::io::Write;
    const MAX_DIAG_BYTES: u64 = 10 * 1024 * 1024;
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".tersh");
    let _ = std::fs::create_dir_all(&base);
    let path = base.join("diag.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_DIAG_BYTES {
            let rotated = base.join("diag.log.1");
            let _ = std::fs::rename(&path, rotated);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    #[cfg(unix)]
    if let Ok(file) = opened.as_ref() {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    let _ = opened.and_then(|mut f| writeln!(f, "{now:.3} {line}"));
}

/// Tauri command — frontend calls this to log a diag line to the same file
/// the Rust backend writes to. Keeps the full pipeline in one place.
#[tauri::command]
pub fn diag_log(message: String) -> AppResult<()> {
    diag_write(&format!("FE {message}"));
    Ok(())
}

#[tauri::command]
pub async fn resize_pty(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> AppResult<()> {
    validate_pty_size(cols, rows)?;
    if let Ok(session) = state.sessions.get(&session_id).await {
        return session.resize(cols, rows).await;
    }
    if let Some(session) = state.local_terminals.get(&session_id).await {
        return session.resize(cols, rows);
    }
    Err(AppError::SessionNotFound(session_id))
}

#[derive(Serialize)]
pub struct UploadCommandResult {
    pub remote_path: String,
    pub bytes_written: u64,
    pub formatted_for_agent: String,
    pub detected_agent: Option<AgentKind>,
}

async fn canonicalize_upload_source(local_path: &str) -> AppResult<String> {
    let canonical: PathBuf = tokio::fs::canonicalize(local_path)
        .await
        .map_err(|e| AppError::Invalid(format!("upload source unavailable: {e}")))?;
    let canonical_str = canonical.to_string_lossy().into_owned();
    if let Some(reason) = sftp::sensitive_path_reason(&canonical_str) {
        // The renderer is not trusted to approve sensitive uploads. Check the
        // canonical path immediately before opening so a symlink swap or direct
        // IPC call cannot bypass the sensitive-file guard.
        return Err(AppError::Invalid(format!("sensitive_path:{reason}")));
    }
    Ok(canonical_str)
}

async fn canonicalize_upload_folder_source(local_path: &str) -> AppResult<String> {
    let canonical: PathBuf = tokio::fs::canonicalize(local_path)
        .await
        .map_err(|e| AppError::Invalid(format!("upload folder unavailable: {e}")))?;
    let meta = tokio::fs::symlink_metadata(&canonical)
        .await
        .map_err(|e| AppError::Invalid(format!("stat upload folder: {e}")))?;
    if !meta.is_dir() {
        return Err(AppError::Invalid("local path is not a folder".into()));
    }
    let canonical_str = canonical.to_string_lossy().into_owned();
    if let Some(reason) = sftp::sensitive_path_reason(&canonical_str) {
        return Err(AppError::Invalid(format!("sensitive_path:{reason}")));
    }
    Ok(canonical_str)
}

fn agent_inbox_dir(session_id: &str) -> String {
    let suffix: String = session_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .take(36)
        .collect();
    let suffix = if suffix.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        suffix
    };
    format!("/tmp/tersh-agent-inbox/{suffix}")
}

/// Validate a frontend-supplied preferred upload directory (the live shell cwd
/// reported via OSC 7). Must be a clean absolute path — reject control chars and
/// anything relative so a hostile server's forged OSC 7 can't redirect uploads
/// somewhere unexpected. Returns the trimmed path, or None to fall back.
fn sanitize_preferred_dir(dir: &str) -> Option<String> {
    let d = dir.trim();
    if !d.starts_with('/') || d.len() > 4096 {
        return None;
    }
    if d.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(d.trim_end_matches('/').to_string())
}

/// Where to drop an upload when no AI agent cwd is available: prefer the live
/// shell cwd (so the file lands where the user actually is), else a neutral
/// per-session inbox under /tmp.
fn fallback_upload_dir(preferred_dir: &Option<String>, session_id: &str) -> String {
    preferred_dir
        .as_deref()
        .and_then(sanitize_preferred_dir)
        .unwrap_or_else(|| agent_inbox_dir(session_id))
}

#[tauri::command]
pub async fn sftp_upload_local(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    host_label: String,
    transfer_id: Option<String>,
    preferred_dir: Option<String>,
) -> AppResult<UploadCommandResult> {
    let canonical_local_path = canonicalize_upload_source(&local_path).await?;
    if tokio::fs::metadata(&canonical_local_path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return sftp_upload_folder_local(
            app,
            state,
            session_id,
            canonical_local_path,
            host_label,
            transfer_id,
            preferred_dir,
        )
        .await;
    }
    let _ = host_label;
    let session = state.sessions.get(&session_id).await?;
    let detected = session.cached_or_detect_agent().await.unwrap_or(None);
    let agent_kind = detected.map(|(kind, _)| kind);
    let (upload_dir, upload_dir_is_agent_cwd) = if let Some((_, agent_pid)) = detected {
        match agent_detect::agent_cwd(&session, agent_pid).await {
            Some(cwd) => (cwd, true),
            None => (fallback_upload_dir(&preferred_dir, &session_id), false),
        }
    } else {
        (fallback_upload_dir(&preferred_dir, &session_id), false)
    };
    let cancel_flag = if let Some(id) = transfer_id.as_ref() {
        Some(state.transfers.register(id.clone()).await)
    } else {
        None
    };
    let progress = transfer_id.clone().map(|id| (app.clone(), id));
    let transfer_sftp =
        match connect_transfer_sftp_for_session(app.clone(), &state, &session_id).await {
            Ok(transfer_sftp) => transfer_sftp,
            Err(e) => {
                if let Some(id) = transfer_id.as_ref() {
                    state.transfers.unregister(id).await;
                }
                return Err(e);
            }
        };
    let upload = sftp::upload_to_agent_dir(
        transfer_sftp.sftp(),
        &canonical_local_path,
        &upload_dir,
        upload_dir_is_agent_cwd,
        progress,
        cancel_flag,
    )
    .await;
    let disconnect_result = transfer_sftp.disconnect().await;
    if let Some(id) = transfer_id.as_ref() {
        state.transfers.unregister(id).await;
    }
    if let Err(e) = disconnect_result {
        tracing::warn!("background transfer sftp disconnect failed: {e}");
    }
    let result = upload?;
    // Always reference the full remote path so the user sees exactly where the
    // file landed. Cleaner-but-shorter `@basename` was confusing — operators
    // want to verify the destination at a glance.
    let formatted = agent_kind
        .map(|kind| kind.format_path(&result.remote_path))
        .unwrap_or_else(|| format!("@{}", result.remote_path));
    Ok(UploadCommandResult {
        remote_path: result.remote_path,
        bytes_written: result.bytes_written,
        formatted_for_agent: formatted,
        detected_agent: agent_kind,
    })
}

#[tauri::command]
pub async fn sftp_upload_folder_local(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    host_label: String,
    transfer_id: Option<String>,
    preferred_dir: Option<String>,
) -> AppResult<UploadCommandResult> {
    let _ = host_label;
    let canonical_local_path = canonicalize_upload_folder_source(&local_path).await?;
    let session = state.sessions.get(&session_id).await?;
    let detected = session.cached_or_detect_agent().await.unwrap_or(None);
    let agent_kind = detected.map(|(kind, _)| kind);
    let (upload_dir, upload_dir_is_agent_cwd) = if let Some((_, agent_pid)) = detected {
        match agent_detect::agent_cwd(&session, agent_pid).await {
            Some(cwd) => (cwd, true),
            None => (fallback_upload_dir(&preferred_dir, &session_id), false),
        }
    } else {
        (fallback_upload_dir(&preferred_dir, &session_id), false)
    };
    let cancel_flag = if let Some(id) = transfer_id.as_ref() {
        Some(state.transfers.register(id.clone()).await)
    } else {
        None
    };
    let progress = transfer_id.clone().map(|id| (app.clone(), id));
    let transfer_sftp =
        match connect_transfer_sftp_for_session(app.clone(), &state, &session_id).await {
            Ok(transfer_sftp) => transfer_sftp,
            Err(e) => {
                if let Some(id) = transfer_id.as_ref() {
                    state.transfers.unregister(id).await;
                }
                return Err(e);
            }
        };
    let upload = sftp::upload_folder_to_agent_dir(
        transfer_sftp.sftp(),
        &canonical_local_path,
        &upload_dir,
        upload_dir_is_agent_cwd,
        progress,
        cancel_flag,
    )
    .await;
    let disconnect_result = transfer_sftp.disconnect().await;
    if let Some(id) = transfer_id.as_ref() {
        state.transfers.unregister(id).await;
    }
    if let Err(e) = disconnect_result {
        tracing::warn!("background transfer sftp disconnect failed: {e}");
    }
    let result = upload?;
    let formatted = agent_kind
        .map(|kind| kind.format_path(&result.remote_path))
        .unwrap_or_else(|| format!("@{}", result.remote_path));
    Ok(UploadCommandResult {
        remote_path: result.remote_path,
        bytes_written: result.bytes_written,
        formatted_for_agent: formatted,
        detected_agent: agent_kind,
    })
}

/// Encrypted JSON export of the whole vault. Caller writes the returned string
/// to a `.json` file; the file is unreadable without the passphrase.
#[tauri::command]
pub async fn export_vault(state: State<'_, AppState>, passphrase: String) -> AppResult<String> {
    if passphrase.len() < 8 {
        return Err(AppError::Invalid(
            "export passphrase must be at least 8 chars".into(),
        ));
    }
    let vault = state.vault.lock().await;
    let dump = vault.dump_all()?;
    let plain =
        serde_json::to_vec(&dump).map_err(|e| AppError::Vault(format!("serialize dump: {e}")))?;
    crate::vault::crypto::passphrase_encrypt(&passphrase, &plain)
}

#[tauri::command]
pub async fn import_vault(
    state: State<'_, AppState>,
    envelope: String,
    passphrase: String,
) -> AppResult<()> {
    let plain = crate::vault::crypto::passphrase_decrypt(&passphrase, &envelope)?;
    let dump: serde_json::Value =
        serde_json::from_slice(&plain).map_err(|e| AppError::Vault(format!("dump parse: {e}")))?;
    let vault = state.vault.lock().await;
    vault.restore_all(&dump)
}

#[tauri::command]
pub async fn export_vault_to_file(
    state: State<'_, AppState>,
    passphrase: String,
    path: String,
) -> AppResult<()> {
    validate_export_file_path(&path)?;
    let envelope = export_vault(state, passphrase).await?;
    tokio::fs::write(&path, envelope)
        .await
        .map_err(|e| AppError::Vault(format!("write export {path}: {e}")))?;
    Ok(())
}

#[tauri::command]
pub async fn import_vault_from_file(
    state: State<'_, AppState>,
    path: String,
    passphrase: String,
) -> AppResult<()> {
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| AppError::Vault(format!("stat import {path}: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Invalid("vault import path must be a file".into()));
    }
    if meta.len() > VAULT_IMPORT_MAX_BYTES {
        return Err(AppError::Invalid(format!(
            "vault import is too large ({} bytes, max {} bytes)",
            meta.len(),
            VAULT_IMPORT_MAX_BYTES
        )));
    }
    let envelope = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::Vault(format!("read import {path}: {e}")))?;
    import_vault(state, envelope, passphrase).await
}

fn validate_export_file_path(path: &str) -> AppResult<()> {
    validate_local_output_file_path(path)
}

const MIN_PTY_COLS: u16 = 10;
const MIN_PTY_ROWS: u16 = 3;
const MAX_PTY_COLS: u16 = 500;
const MAX_PTY_ROWS: u16 = 200;
const MAX_TERMINAL_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_CLIPBOARD_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

fn validate_pty_size(cols: u16, rows: u16) -> AppResult<()> {
    if !(MIN_PTY_COLS..=MAX_PTY_COLS).contains(&cols) {
        return Err(AppError::Invalid(format!(
            "terminal columns must be between {MIN_PTY_COLS} and {MAX_PTY_COLS}"
        )));
    }
    if !(MIN_PTY_ROWS..=MAX_PTY_ROWS).contains(&rows) {
        return Err(AppError::Invalid(format!(
            "terminal rows must be between {MIN_PTY_ROWS} and {MAX_PTY_ROWS}"
        )));
    }
    Ok(())
}

fn validate_terminal_input_len(len: usize) -> AppResult<()> {
    if len > MAX_TERMINAL_INPUT_BYTES {
        return Err(AppError::Invalid(format!(
            "terminal input is too large ({len} bytes, max {MAX_TERMINAL_INPUT_BYTES} bytes)"
        )));
    }
    Ok(())
}

fn validate_local_output_file_path(path: &str) -> AppResult<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." || trimmed == "~" {
        return Err(AppError::Invalid(
            "output path must be an absolute file path".into(),
        ));
    }
    let p = std::path::Path::new(trimmed);
    if !p.is_absolute() {
        return Err(AppError::Invalid("output path must be absolute".into()));
    }
    if path_has_parent_component(p) {
        return Err(AppError::Invalid(
            "output path cannot contain '..' components".into(),
        ));
    }
    if p.parent().is_none() {
        return Err(AppError::Invalid(
            "refusing to write output to filesystem root".into(),
        ));
    }
    if let Ok(meta) = std::fs::symlink_metadata(p) {
        if meta.file_type().is_symlink() {
            return Err(AppError::Invalid(
                "output path cannot be a symbolic link".into(),
            ));
        }
        if meta.is_dir() {
            return Err(AppError::Invalid(
                "output path must be a file, not a directory".into(),
            ));
        }
    }
    if let Some(home) = user_home_dir() {
        if p == home.as_path() {
            return Err(AppError::Invalid(
                "refusing to write output over the home directory".into(),
            ));
        }
    }
    Ok(())
}

// ── SSH KEY PASSPHRASES IN OS KEYCHAIN ───────────────────────────────────────
//
// Service: `tersh-keypass`, account: `<key_id>`. The renderer can store
// a passphrase once; subsequent `connect` calls that need that key passphrase
// look it up via list_active_keypass_keys to decide whether to prompt.

const KEYPASS_SERVICE: &str = "tersh-keypass";
const HOSTPASS_SERVICE: &str = "tersh-hostpass";
const VAULT_IMPORT_MAX_BYTES: u64 = 25 * 1024 * 1024;

fn legacy_keypass_service() -> String {
    ["open", "ter", "mius", "-keypass"].concat()
}

fn legacy_hostpass_service() -> String {
    ["open", "ter", "mius", "-hostpass"].concat()
}

/// Look up a credential in the current service; on NotFound, fall back to the
/// previous service and migrate the entry over.
fn keychain_get_with_migration(
    service: &str,
    legacy_service: &str,
    account: &str,
) -> AppResult<String> {
    let entry = keyring::Entry::new(service, account)
        .map_err(|e| AppError::Internal(format!("keychain entry: {e}")))?;
    match entry.get_password() {
        Ok(v) => Ok(v),
        Err(keyring::Error::NoEntry) => {
            let legacy = keyring::Entry::new(legacy_service, account)
                .map_err(|e| AppError::Internal(format!("legacy keychain entry: {e}")))?;
            let value = legacy
                .get_password()
                .map_err(|e| AppError::Internal(format!("keychain get: {e}")))?;
            entry
                .set_password(&value)
                .map_err(|e| AppError::Internal(format!("keychain migrate: {e}")))?;
            match legacy.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {
                    tracing::info!(
                        account,
                        service,
                        "migrated keychain entry from legacy service"
                    );
                }
                Err(e) => tracing::warn!(
                    account,
                    service,
                    "migrated keychain entry but could not delete legacy service entry: {e}"
                ),
            }
            Ok(value)
        }
        Err(e) => Err(AppError::Internal(format!("keychain get: {e}"))),
    }
}

fn get_host_password_from_keychain(host_id: &str) -> AppResult<String> {
    let legacy_service = legacy_hostpass_service();
    keychain_get_with_migration(HOSTPASS_SERVICE, &legacy_service, host_id)
}

async fn get_host_password(
    state: &State<'_, AppState>,
    host_id: &str,
) -> AppResult<Option<String>> {
    if let Ok(password) = get_host_password_from_keychain(host_id) {
        return Ok(Some(password));
    }
    let vault = state.vault.lock().await;
    vault.get_host_password(host_id)
}

#[tauri::command]
pub async fn set_host_password(
    state: State<'_, AppState>,
    host_id: String,
    password: String,
) -> AppResult<()> {
    {
        let vault = state.vault.lock().await;
        vault.set_host_password(&host_id, &password)?;
    }
    if let Ok(entry) = keyring::Entry::new(HOSTPASS_SERVICE, &host_id) {
        if let Err(e) = entry.set_password(&password) {
            tracing::warn!(
                host_id,
                "could not save host password to macOS keychain: {e}"
            );
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn clear_host_password(state: State<'_, AppState>, host_id: String) -> AppResult<()> {
    {
        let vault = state.vault.lock().await;
        vault.clear_host_password(&host_id)?;
    }
    if let Ok(entry) = keyring::Entry::new(HOSTPASS_SERVICE, &host_id) {
        let _ = entry.delete_credential();
    }
    Ok(())
}

#[tauri::command]
pub async fn has_host_password(state: State<'_, AppState>, host_id: String) -> AppResult<bool> {
    Ok(get_host_password(&state, &host_id).await?.is_some())
}

// ════════════════════════════════════════════════════════════════════════════
// PROMPT ENHANCER
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PromptEnhancerProvider {
    Openrouter,
    Deepseek,
    Mimo,
    Custom,
}

#[derive(Debug, Deserialize)]
pub struct PromptEnhanceRequest {
    pub provider: PromptEnhancerProvider,
    pub base_url: Option<String>,
    pub api_key: String,
    pub model: String,
    pub prompt: String,
    pub brain_id: Option<String>,
    pub session_id: Option<String>,
    /// Optional embedding model. When present (and non-empty),
    /// retrieve_context_with_trace will embed the prompt to enable
    /// semantic-search blending in score_file_v2.
    #[serde(default)]
    pub embedding_model: Option<String>,
}

impl PromptEnhanceRequest {
    pub fn to_embedding_config(&self) -> Option<crate::brain::embed::EmbeddingConfig> {
        let model = self.embedding_model.as_deref()?.trim();
        if model.is_empty() {
            return None;
        }
        if matches!(self.provider, PromptEnhancerProvider::Deepseek) {
            return None;
        }
        let provider = match self.provider {
            PromptEnhancerProvider::Openrouter => "openrouter",
            PromptEnhancerProvider::Mimo => "mimo",
            PromptEnhancerProvider::Custom => "custom",
            PromptEnhancerProvider::Deepseek => return None,
        };
        Some(crate::brain::embed::EmbeddingConfig {
            provider: provider.to_string(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            embedding_model: model.to_string(),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct PromptEnhanceResponse {
    pub enhanced_prompt: String,
    pub interpretation: Option<String>,
    pub used_project_context: bool,
    pub prompt_intent: crate::brain::index::PromptIntentKind,
    pub context_reason: String,
    pub project_context_available: bool,
    pub provider: PromptEnhancerProvider,
    pub model: String,
    pub tool_calls_used: u32,
    pub context_trace: Vec<PromptContextTraceItem>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct PromptContextTraceItem {
    pub tool: String,
    pub target: Option<String>,
    pub status: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrainIndexAiConfig {
    pub provider: PromptEnhancerProvider,
    pub base_url: Option<String>,
    pub api_key: String,
    pub model: String,
    /// Optional embedding model. Empty/absent = TF-IDF + n-gram retrieval only.
    /// Set explicitly to enable semantic search via the same provider's
    /// /embeddings endpoint. Per CLAUDE.md, NEVER silently defaulted.
    #[serde(default)]
    pub embedding_model: Option<String>,
}

impl BrainIndexAiConfig {
    /// Convert to an EmbeddingConfig if the user opted in by setting a
    /// non-empty embedding_model. DeepSeek direct has no embeddings API
    /// so it returns None even if the field was somehow set.
    pub fn to_embedding_config(&self) -> Option<crate::brain::embed::EmbeddingConfig> {
        let model = self.embedding_model.as_deref()?.trim();
        if model.is_empty() {
            return None;
        }
        if matches!(self.provider, PromptEnhancerProvider::Deepseek) {
            return None;
        }
        let provider = match self.provider {
            PromptEnhancerProvider::Openrouter => "openrouter",
            PromptEnhancerProvider::Mimo => "mimo",
            PromptEnhancerProvider::Custom => "custom",
            PromptEnhancerProvider::Deepseek => return None,
        };
        Some(crate::brain::embed::EmbeddingConfig {
            provider: provider.to_string(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            embedding_model: model.to_string(),
        })
    }
}

#[derive(Deserialize, Debug)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize, Debug)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize, Debug)]
struct ChatChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCall>>,
    /// DeepSeek's reasoning models (v4-pro / -reasoner) return chain-of-thought
    /// here. The API REQUIRES it on every echoed-back assistant message in
    /// thinking mode — dropping it returns 400 "The `reasoning_content` in
    /// the thinking mode must be passed back to the API."
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    function: ChatToolCallFunction,
}

#[derive(Deserialize, Debug, Clone)]
struct ChatToolCallFunction {
    name: String,
    /// Provider returns this as a JSON string; we parse it into a Value
    /// before dispatching to the executor.
    arguments: String,
}

async fn read_provider_response_text(
    response: reqwest::Response,
    label: &str,
) -> AppResult<(reqwest::StatusCode, String)> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("{label} response read: {e}")))?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

const PROMPT_ENHANCER_MAX_PROMPT_CHARS: usize = 12_000;
const PROMPT_ENHANCER_MAX_KEY_CHARS: usize = 4096;
// Total per-request timeout (reqwest .timeout() covers connect + the WAIT for
// the model + body read). Non-streaming, so we hold the connection open the
// whole time the model thinks. DeepSeek on max reasoning_effort with a large
// max_tokens regularly thinks 1–3 min on a complex prompt; 90s was too short and
// surfaced as "response read: error decoding response body" (a body-read timeout,
// NOT a decode failure — gzip/brotli/zstd/deflate are all disabled). 300s gives
// max-thinking real headroom; a wrong key / bad request still fails fast (4xx).
const PROMPT_ENHANCER_TIMEOUT_SECS: u64 = 300;
const PROMPT_INDEX_DIGEST_TIMEOUT_SECS: u64 = 180;
/// Cap on agent-loop tool calls so a model that wants to keep exploring can't
/// burn the user's tokens forever. Hitting it is NOT an error — the loop stops
/// offering tools and forces a final answer (see the budget check below).
/// Generous enough for max-thinking grounding (read several files + grep/tree).
const PROMPT_ENHANCER_MAX_ROUNDS: u32 = 20;

/// app_secrets key for the prompt-enhancer provider API key. Stored encrypted
/// at rest in the vault so it survives launches (no more re-typing every run).
const PROMPT_ENHANCER_KEY_SECRET: &str = "prompt_enhancer_api_key";

#[tauri::command]
pub async fn prompt_enhancer_get_api_key(state: State<'_, AppState>) -> AppResult<Option<String>> {
    let vault = state.vault.lock().await;
    vault.get_app_secret(PROMPT_ENHANCER_KEY_SECRET)
}

#[tauri::command]
pub async fn prompt_enhancer_set_api_key(
    state: State<'_, AppState>,
    api_key: String,
) -> AppResult<()> {
    let trimmed = api_key.trim();
    let vault = state.vault.lock().await;
    if trimmed.is_empty() {
        vault.clear_app_secret(PROMPT_ENHANCER_KEY_SECRET)
    } else {
        if trimmed.len() > PROMPT_ENHANCER_MAX_KEY_CHARS {
            return Err(AppError::Invalid(format!(
                "API key too long (max {PROMPT_ENHANCER_MAX_KEY_CHARS} chars)"
            )));
        }
        vault.set_app_secret(PROMPT_ENHANCER_KEY_SECRET, trimmed)
    }
}

#[tauri::command]
pub async fn prompt_enhance(
    state: State<'_, AppState>,
    req: PromptEnhanceRequest,
) -> AppResult<PromptEnhanceResponse> {
    validate_prompt_enhance_request(&req)?;
    let model = normalize_prompt_enhancer_model(req.provider, req.model.trim())?;
    let base_url = prompt_provider_base_url(req.provider, req.base_url.as_deref())?;
    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let scope = detect_project_scope(&state, &req).await?;
    let has_project_index = match &scope {
        Some(s) => state.brain.get_by_scope(s).await.is_some(),
        None => false,
    };
    let context_decision = crate::brain::index::decide_project_context(&req.prompt);
    // Retrieval-by-default: if a project index is selected, USE it and let the
    // model decide (via tools) whether to read the repo. Only skip for clearly
    // from-scratch prompts, where repo context would pollute a new project. The
    // old gate also required a brittle keyword match (`use_project_context`), so
    // a debugging prompt like "not work well" missed the bug keyword and lost
    // all context — exactly the failure we're killing here.
    let brain_enabled = has_project_index
        && context_decision.kind != crate::brain::index::PromptIntentKind::FreshBuild;
    if let Some(scope) = &scope {
        if brain_enabled {
            let id = crate::brain::BrainId::from_scope_key(&scope.scope_key());
            state.brain.touch_used(&id).await;
            if let Err(e) =
                refresh_prompt_scope_index_if_expired(&state, scope, req.session_id.as_deref())
                    .await
            {
                tracing::warn!("prompt enhancer index refresh skipped: {e}");
            }
        }
    }

    let embed_for_retrieve = req.to_embedding_config();
    let initial_user = build_user_message(
        &req.prompt,
        scope.as_ref(),
        brain_enabled,
        &context_decision,
        has_project_index,
        embed_for_retrieve.as_ref(),
    )
    .await;
    let mut context_trace = initial_user.context_trace;
    let mut messages: Vec<serde_json::Value> = vec![
        json_msg("system", prompt_enhancer_system_prompt(brain_enabled)),
        json_msg("user", &initial_user.message),
    ];

    let tools = if brain_enabled {
        Some(crate::brain::tools::tool_schemas())
    } else {
        None
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(PROMPT_ENHANCER_TIMEOUT_SECS))
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .build()
        .map_err(|e| AppError::Internal(format!("prompt enhancer client: {e}")))?;

    let mut tool_calls_used: u32 = 0;
    // When the tool budget is reached we don't fail — flip this on, drop tools
    // from the request, and make one final tools-less call that forces the
    // answer with whatever context was already gathered.
    let mut force_final = false;
    // DeepSeek gets the big thinking budget; other providers get a modest cap
    // that won't 400 small-output models. Empty content (max-thinking's reasoning
    // ate the output cap) flips `max_tokens_big` on (sticky) and retries, capped.
    let is_deepseek = matches!(req.provider, PromptEnhancerProvider::Deepseek)
        || model.to_ascii_lowercase().contains("deepseek");
    let mut max_tokens_big = false;
    let mut empty_retries: u8 = 0;
    let final_text = loop {
        if messages.len() > 64 {
            return Err(AppError::Invalid(
                "agent ran past message-history limit".into(),
            ));
        }
        // Per DeepSeek V4 docs: reasoning tokens COUNT toward max_tokens (max
        // output is 384K). At Think Max the CoT alone can be many thousands of
        // tokens, so a small cap truncates MID-REASONING -> empty content. Give
        // it real room (32K), 64K on the retry. It's a ceiling, not a target:
        // the model stops when done, so a short answer still costs little.
        // Non-DeepSeek models (OpenRouter/custom/local) often cap output well
        // below 32K and would 400 — give them a safe modest cap. DeepSeek gets
        // real room for max-effort reasoning (bumped to 64K after an empty pass).
        let max_tokens = if !is_deepseek {
            4096
        } else if max_tokens_big {
            64000
        } else {
            32000
        };
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "temperature": 0.25,
            "max_tokens": max_tokens,
        });
        if !force_final {
            if let Some(t) = tools.as_ref() {
                body["tools"] = serde_json::Value::Array(t.clone());
                body["tool_choice"] = serde_json::Value::String("auto".into());
            }
        }
        // DeepSeek V4 thinking mode at MAX effort — force the enhancer to reason
        // about the user's TRUE intent before writing (e.g. "build a logo
        // service" ≠ "call Clearbit"). Thinking defaults to "high"; we push it to
        // "max". reasoning_content is round-tripped (above) and never shown.
        // Scoped to DeepSeek so the fields don't 400 on providers that reject
        // unknown body keys; covers DeepSeek-direct and deepseek models via
        // OpenRouter/custom. In thinking mode temperature is ignored (no effect).
        if is_deepseek {
            // Max thinking stays ON on every pass — the empty-content retry only
            // raises the output budget, it never sacrifices thinking.
            body["thinking"] = serde_json::json!({ "type": "enabled" });
            body["reasoning_effort"] = serde_json::Value::String("max".into());
        }

        let mut builder = client
            .post(&endpoint)
            .bearer_auth(req.api_key.trim())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .json(&body);
        if matches!(req.provider, PromptEnhancerProvider::Openrouter) {
            builder = builder
                .header("HTTP-Referer", "https://tersh.app")
                .header("X-Title", "Tersh");
        }
        let response = builder
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("prompt enhancer request failed: {e}")))?;
        let (status, text) = read_provider_response_text(response, "prompt enhancer").await?;
        if !status.is_success() {
            let short = text.chars().take(400).collect::<String>();
            return Err(AppError::Invalid(format!(
                "prompt enhancer provider returned {status}: {short}"
            )));
        }
        let parsed: ChatResponse = serde_json::from_str(&text)
            .map_err(|e| AppError::Internal(format!("prompt enhancer response parse: {e}")))?;
        let assistant = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| AppError::Invalid("prompt enhancer returned no choices".into()))?;

        let calls = assistant.tool_calls.unwrap_or_default();
        if calls.is_empty() {
            let final_content = assistant.content.unwrap_or_default().trim().to_string();
            if final_content.is_empty() {
                // Empty content under max-thinking = reasoning ate the output cap
                // (not the 1M context). Bump to the big budget (sticky) and retry,
                // capped at 2 so it can't spin. Reachable on any turn, so an early
                // empty turn doesn't burn the only chance a later one needs.
                if is_deepseek && empty_retries < 2 {
                    empty_retries += 1;
                    max_tokens_big = true;
                    continue;
                }
                return Err(AppError::Invalid(
                    "prompt enhancer returned empty text".into(),
                ));
            }
            break final_content;
        }

        // We already withdrew tools (force_final) but the model STILL emitted
        // tool calls. The request carried NO tools, so echoing a tool_calls
        // message would 400 the next turn — instead finalize with its text, or
        // error. Hoisted OUT of the budget check so it fires for ANY non-empty
        // calls while force_final is set (tool_calls_used may not have advanced).
        if force_final {
            let txt = assistant.content.unwrap_or_default().trim().to_string();
            if !txt.is_empty() {
                break txt;
            }
            return Err(AppError::Invalid(
                "prompt enhancer could not finalize within the tool budget".into(),
            ));
        }

        // Tool budget reached — DON'T fail. Discard this exploratory turn, stop
        // offering tools, and ask for the final answer with what was gathered.
        // Next iteration omits tools so the model must produce the two headings.
        if tool_calls_used + (calls.len() as u32) > PROMPT_ENHANCER_MAX_ROUNDS {
            force_final = true;
            messages.push(serde_json::json!({
                "role": "user",
                "content": "Tool-call budget reached — do NOT call any more tools. Using the context you've already gathered, output the final result NOW: exactly the two headings (Interpretation, then Enhanced prompt) and nothing else."
            }));
            continue;
        }

        // Normalize tool-call ids before echoing. Providers occasionally return
        // an empty or duplicated `id`; the next request 400s if a `tool` message
        // references an id that isn't present (and unique) in the assistant
        // message's `tool_calls`. Synthesize a stable, unique id where needed and
        // use the SAME id in both the echo and the matching tool result.
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let normalized: Vec<(String, ChatToolCall)> = calls
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let mut id = c.id.clone();
                if id.is_empty() || !seen_ids.insert(id.clone()) {
                    // Synthesize until we land on an id not already taken by a
                    // real provider id or a prior synth — a bare insert() whose
                    // result is ignored can still collide with a real `call_N_i`.
                    let mut n = i;
                    let cand = loop {
                        let cand = format!("call_{tool_calls_used}_{n}");
                        if seen_ids.insert(cand.clone()) {
                            break cand;
                        }
                        n += 1;
                    };
                    id = cand;
                }
                (id, c)
            })
            .collect();

        // Echo the assistant message back so the provider has the call IDs.
        // DeepSeek thinking-mode also requires reasoning_content round-tripped.
        let mut assistant_msg = serde_json::json!({
            "role": "assistant",
            "content": assistant.content.unwrap_or_default(),
            "tool_calls": normalized.iter().map(|(id, c)| serde_json::json!({
                "id": id,
                "type": c.kind.clone().unwrap_or_else(|| "function".to_string()),
                "function": { "name": c.function.name, "arguments": c.function.arguments }
            })).collect::<Vec<_>>(),
        });
        if let Some(rc) = assistant.reasoning_content {
            assistant_msg["reasoning_content"] = serde_json::Value::String(rc);
        }
        messages.push(assistant_msg);

        for (call_id, call) in normalized {
            tool_calls_used += 1;
            let tool_name = call.function.name.clone();
            let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            let trace_target = summarize_prompt_tool_call(&tool_name, &args);
            let result = match scope.as_ref() {
                Some(s) => {
                    crate::brain::tools::execute_for_scope(s, &tool_name, &args, &state.sessions)
                        .await
                }
                None => Err(AppError::Invalid(
                    "tool call without an active project scope".into(),
                )),
            };
            let (status, content) = match result {
                Ok(s) => ("ok".to_string(), truncate_for_chat(&s)),
                Err(e) => ("error".to_string(), format!("[tool error] {e}")),
            };
            context_trace.push(PromptContextTraceItem {
                tool: tool_name,
                target: trace_target,
                status,
            });
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content,
            }));
        }
    };

    let parsed_output = parse_prompt_enhancer_output(&final_text);

    // Reason reflects what actually happened, not the old keyword heuristic.
    let context_reason = if brain_enabled {
        "Using the selected project index to ground the prompt in real files and symbols."
            .to_string()
    } else if has_project_index {
        "Fresh-build prompt — kept general instead of pulling repo context.".to_string()
    } else {
        "No project selected — enhanced without repo context.".to_string()
    };

    Ok(PromptEnhanceResponse {
        enhanced_prompt: parsed_output.enhanced_prompt,
        interpretation: parsed_output.interpretation,
        used_project_context: brain_enabled,
        prompt_intent: context_decision.kind,
        context_reason,
        project_context_available: has_project_index,
        provider: req.provider,
        model,
        tool_calls_used,
        context_trace,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPromptEnhancement {
    enhanced_prompt: String,
    interpretation: Option<String>,
}

fn parse_prompt_enhancer_output(raw: &str) -> ParsedPromptEnhancement {
    let trimmed = raw.trim();
    let lines = trimmed.lines().collect::<Vec<_>>();
    let mut interpretation_idx = None;
    let mut enhanced_idx = None;
    // We still LOCATE a stray "Questions" heading — not to use it, but to cut it
    // OFF the enhanced prompt. The enhancer is instructed never to ask, so if a
    // model emits one anyway we drop it rather than leak it into the rewrite.
    let mut questions_idx = None;

    for (idx, line) in lines.iter().enumerate() {
        let heading = normalize_prompt_output_heading(line);
        if heading == "interpretation" {
            interpretation_idx = Some(idx);
        } else if matches!(
            heading.as_str(),
            "enhanced prompt" | "enhanced task" | "rewritten prompt"
        ) {
            enhanced_idx = Some(idx);
        } else if matches!(
            heading.as_str(),
            "questions" | "clarifying questions" | "open questions"
        ) {
            questions_idx = Some(idx);
        }
    }

    match (interpretation_idx, enhanced_idx, questions_idx) {
        (Some(i), Some(e), q) if i < e => {
            let interpretation_end = q.filter(|q_idx| *q_idx > i && *q_idx < e).unwrap_or(e);
            let enhanced_end = q.filter(|q_idx| *q_idx > e).unwrap_or(lines.len());
            // Clamp Interpretation to a single line at PARSE time — the system
            // prompt asks for one sentence, but a thinking model may ignore that
            // and dump a paragraph of reasoning here. Defensive clamp keeps the
            // box showing a tight task summary, never the model's chain-of-thought.
            let interpretation = lines[i + 1..interpretation_end]
                .iter()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            let enhanced_prompt = lines[e + 1..enhanced_end].join("\n").trim().to_string();
            ParsedPromptEnhancement {
                enhanced_prompt: if enhanced_prompt.is_empty() {
                    trimmed.to_string()
                } else {
                    enhanced_prompt
                },
                interpretation: if interpretation.is_empty() {
                    None
                } else {
                    Some(interpretation)
                },
            }
        }
        (_, Some(e), q) => {
            let enhanced_end = q.filter(|q_idx| *q_idx > e).unwrap_or(lines.len());
            let enhanced_prompt = lines[e + 1..enhanced_end].join("\n").trim().to_string();
            ParsedPromptEnhancement {
                enhanced_prompt: if enhanced_prompt.is_empty() {
                    trimmed.to_string()
                } else {
                    enhanced_prompt
                },
                interpretation: None,
            }
        }
        _ => ParsedPromptEnhancement {
            enhanced_prompt: trimmed.to_string(),
            interpretation: None,
        },
    }
}

fn summarize_prompt_tool_call(name: &str, args: &serde_json::Value) -> Option<String> {
    let target = match name {
        "read_file" => prompt_trace_arg(args, "path"),
        "list_directory" | "tree" => prompt_trace_arg(args, "path").or_else(|| Some(".".into())),
        "find_files" => {
            let query = prompt_trace_arg(args, "query")?;
            match prompt_trace_arg(args, "path") {
                Some(path) if path != "." => Some(format!("{query} in {path}")),
                _ => Some(query),
            }
        }
        "grep" => {
            let pattern = prompt_trace_arg(args, "pattern")?;
            match prompt_trace_arg(args, "path") {
                Some(path) if path != "." => Some(format!("{pattern} in {path}")),
                _ => Some(pattern),
            }
        }
        _ => None,
    }?;

    Some(sanitize_prompt_trace_target(&target))
}

fn prompt_trace_arg(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn sanitize_prompt_trace_target(value: &str) -> String {
    const MAX_TRACE_TARGET_CHARS: usize = 120;
    let mut out = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if out.chars().count() > MAX_TRACE_TARGET_CHARS {
        out = out
            .chars()
            .take(MAX_TRACE_TARGET_CHARS.saturating_sub(1))
            .collect::<String>();
        out.push('…');
    }

    out
}

fn normalize_prompt_output_heading(line: &str) -> String {
    line.trim()
        .trim_start_matches('#')
        .trim_start_matches('*')
        .trim_start_matches('-')
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_lowercase()
}

fn json_msg(role: &str, content: &str) -> serde_json::Value {
    serde_json::json!({"role": role, "content": content})
}

fn truncate_for_chat(text: &str) -> String {
    const MAX: usize = 32 * 1024;
    if text.len() <= MAX {
        text.to_string()
    } else {
        let mut t: String = text.chars().take(MAX).collect();
        t.push_str("\n[…truncated by tersh]");
        t
    }
}

fn validate_prompt_enhance_request(req: &PromptEnhanceRequest) -> AppResult<()> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(AppError::Invalid("prompt is empty".into()));
    }
    if prompt.chars().count() > PROMPT_ENHANCER_MAX_PROMPT_CHARS {
        return Err(AppError::Invalid("prompt is too large".into()));
    }
    if req.api_key.trim().is_empty() {
        return Err(AppError::Invalid(
            "prompt enhancer API key is required".into(),
        ));
    }
    if req.api_key.chars().count() > PROMPT_ENHANCER_MAX_KEY_CHARS {
        return Err(AppError::Invalid(
            "prompt enhancer API key is too large".into(),
        ));
    }
    if req.model.trim().is_empty() || req.model.chars().count() > 160 {
        return Err(AppError::Invalid("prompt enhancer model is invalid".into()));
    }
    Ok(())
}

fn normalize_prompt_enhancer_model(
    provider: PromptEnhancerProvider,
    raw_model: &str,
) -> AppResult<String> {
    let model = raw_model.trim();
    if model.is_empty() {
        return Err(AppError::Invalid("prompt enhancer model is invalid".into()));
    }

    if !matches!(provider, PromptEnhancerProvider::Deepseek) {
        return Ok(model.to_string());
    }

    let normalized = match model {
        // Legacy DeepSeek aliases from earlier API generations. Keep these as
        // migrations only; direct DeepSeek now documents v4 model names.
        "deepseek-chat" => "deepseek-v4-flash",
        "deepseek-reasoner" => "deepseek-v4-pro",
        other => other,
    };

    match normalized {
        "deepseek-v4-flash" | "deepseek-v4-pro" => Ok(normalized.to_string()),
        _ => Err(AppError::Invalid(
            "DeepSeek direct supports deepseek-v4-flash or deepseek-v4-pro".into(),
        )),
    }
}

fn prompt_provider_base_url(
    provider: PromptEnhancerProvider,
    custom: Option<&str>,
) -> AppResult<String> {
    let raw = match provider {
        PromptEnhancerProvider::Openrouter => custom
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("https://openrouter.ai/api/v1"),
        PromptEnhancerProvider::Deepseek => custom
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("https://api.deepseek.com"),
        PromptEnhancerProvider::Mimo | PromptEnhancerProvider::Custom => custom
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AppError::Invalid("base URL is required for this provider".into()))?,
    }
    .trim()
    .trim_end_matches('/')
    .to_string();

    if !(raw.starts_with("https://")
        || raw.starts_with("http://localhost")
        || raw.starts_with("http://127.0.0.1"))
    {
        return Err(AppError::Invalid(
            "prompt enhancer base URL must be https or localhost".into(),
        ));
    }
    Ok(raw)
}

async fn synthesize_project_digest(
    config: &BrainIndexAiConfig,
    index: &crate::brain::index::ProjectIndex,
) -> AppResult<Option<String>> {
    if config.api_key.trim().is_empty() {
        return Err(AppError::Invalid(
            "project index AI digest requires a provider API key".into(),
        ));
    }
    if config.api_key.chars().count() > PROMPT_ENHANCER_MAX_KEY_CHARS {
        return Err(AppError::Invalid(
            "project index API key is too large".into(),
        ));
    }
    let model = normalize_prompt_enhancer_model(config.provider, config.model.trim())?;
    let base_url = prompt_provider_base_url(config.provider, config.base_url.as_deref())?;
    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let payload = build_project_digest_payload(index);

    let messages = vec![
        json_msg(
            "system",
            "You are Tersh Project Indexer. Create a compact, accurate project digest for future coding-agent prompt enhancement. Use only the provided index payload. Do not invent files, frameworks, product claims, credentials, IPs, or deadlines. Prefer concrete architecture, modules, workflows, validation commands, and gotchas that help the next agent understand the repo fast.",
        ),
        json_msg(
            "user",
            &format!(
                "Write the stored project digest for this selected project.\n\n{}\n\nReturn only the digest, as 6-10 concise bullets.",
                payload
            ),
        ),
    ];

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            PROMPT_INDEX_DIGEST_TIMEOUT_SECS,
        ))
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .build()
        .map_err(|e| AppError::Internal(format!("project index client: {e}")))?;
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": 0.15,
        "max_tokens": 1200,
    });
    // Same DeepSeek max-thinking treatment for the project-summary synthesis, so
    // the digest is reasoned about, not pattern-matched. One-shot call, so no
    // reasoning_content round-trip needed; the field is ignored.
    if matches!(config.provider, PromptEnhancerProvider::Deepseek)
        || model.to_ascii_lowercase().contains("deepseek")
    {
        body["thinking"] = serde_json::json!({ "type": "enabled" });
        body["reasoning_effort"] = serde_json::Value::String("max".into());
    }
    let mut builder = client
        .post(&endpoint)
        .bearer_auth(config.api_key.trim())
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .json(&body);
    if matches!(config.provider, PromptEnhancerProvider::Openrouter) {
        builder = builder
            .header("HTTP-Referer", "https://tersh.app")
            .header("X-Title", "Tersh");
    }
    let response = builder
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("project index request failed: {e}")))?;
    let (status, text) = read_provider_response_text(response, "project index").await?;
    if !status.is_success() {
        let short = text.chars().take(400).collect::<String>();
        return Err(AppError::Invalid(format!(
            "project index provider returned {status}: {short}"
        )));
    }
    extract_project_digest_from_response(&text)
}

fn extract_project_digest_from_response(text: &str) -> AppResult<Option<String>> {
    let parsed: ChatResponse = serde_json::from_str(text)
        .map_err(|e| AppError::Internal(format!("project index response parse: {e}")))?;
    let digest = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .unwrap_or_default()
        .trim()
        .to_string();
    if digest.is_empty() {
        return Ok(None);
    }
    Ok(Some(digest))
}

async fn apply_project_digest_synthesis(
    config: &BrainIndexAiConfig,
    index: &mut crate::brain::index::ProjectIndex,
) {
    match synthesize_project_digest(config, index).await {
        Ok(Some(digest)) => index.project_digest = digest,
        Ok(None) => {
            tracing::warn!("project index provider returned empty digest; keeping local digest")
        }
        Err(e) => tracing::warn!("project index digest synthesis skipped: {e}"),
    }
}

fn build_project_digest_payload(index: &crate::brain::index::ProjectIndex) -> String {
    let files = index
        .files
        .iter()
        .take(80)
        .map(|file| {
            format!(
                "- {} [{}; {}; symbols: {}; imports: {}]: {}",
                file.path,
                file.language,
                file.role,
                file.symbols
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                file.imports
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                file.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    truncate_for_chat(&format!(
        "Project: {}\nRoot hint: {}\nIndexed files: {}; chunks: {}; bytes: {}\n\nDeterministic overview:\n{}\n\nDeterministic digest:\n{}\n\nProject map:\n- Package managers: {}\n- Frameworks: {}\n- Capabilities: {}\n- Architecture: {}\n- Modules: {}\n- Manifests: {}\n- Entrypoints: {}\n- Config files: {}\n- Tests: {}\n- Docs: {}\n- Scripts: {}\n- Dependencies: {}\n\nFile summaries:\n{}",
        index.label,
        index.root_hint,
        index.files_indexed,
        index.chunks_indexed,
        index.total_bytes,
        index.overview,
        index.project_digest,
        index.project_map.package_managers.join(", "),
        index.project_map.frameworks.join(", "),
        index.project_map.capabilities.join(", "),
        index.project_map.architecture.join(", "),
        index.project_map.modules.join(", "),
        index.project_map.manifests.join(", "),
        index.project_map.entrypoints.join(", "),
        index.project_map.config_files.join(", "),
        index.project_map.test_files.join(", "),
        index.project_map.doc_files.join(", "),
        index.project_map.scripts.join(", "),
        index.project_map.dependencies.iter().take(60).cloned().collect::<Vec<_>>().join(", "),
        files,
    ))
}

fn prompt_enhancer_system_prompt(with_tools: bool) -> &'static str {
    if with_tools {
        "You are Tersh Prompt Enhancer — the sharpest prompt rewriter there is. You REWRITE the user's rough prompt into a sharper version of the SAME request for a SEPARATE coding agent — usually a ready-to-run coding task, but you MIRROR the request's shape (see the first rule). You are NOT that agent and NOT a chat assistant: you never answer, explain, or do the work — you only produce a better PROMPT, and you are not talking to the user.\n\
        You have read-only tools — find_files, read_file, list_directory, grep, tree — over the user's selected project. GROUND the rewrite in reality: when the user names a feature, file, function, bug, or behaviour, find the real paths and symbols and reference them BY NAME. Beyond file names, surface the project's EXISTING CONVENTIONS the change should follow — error-handling style, naming, module layout, test framework/pattern — and tell the agent to match them (e.g. 'follow the existing error handling in `errors.rs`', 'add a test in the existing `vitest` style'). Prefer a few targeted reads over broad scans. If this is a fresh-build / from-scratch request, do NOT explore the repo — write a clean general spec.\n\
        Hard rules:\n\
        - MIRROR THE REQUEST'S SHAPE, SHARPEN IT, NEVER FULFILL IT: your output is always a sharper version of the SAME ask the user made, handed to the coding agent — a question becomes a tighter question, a brainstorm becomes a tighter idea-generation brief, a plan/compare/advise becomes a rigorous planning/comparison/decision ask, a build/fix becomes a ready-to-run task. You sharpen WHAT IS ASKED and HOW A GOOD ANSWER IS JUDGED (scope, where to look, criteria, what it must cover); you NEVER supply the answer, the idea list, the plan, the verdict, or the code yourself. If your draft contains the deliverable instead of the refined ask for it, you have failed. (See SHAPE MAP below.)\n\
        - THINK FIRST about the user's TRUE goal and what would DEFEAT it, before writing. If they want to BUILD or MAKE a capability, IMPLEMENT it — never just call or proxy the third-party service that already provides exactly that (building a logo fetcher is NOT 'call Clearbit'; building auth is NOT 'use Auth0'). Favor built-in/native APIs and durable approaches over extra dependencies or deprecated/paid services. Reason silently; output only the two headings.\n\
        - INTERPRETATION ALWAYS NAMES A CONCRETE TASK. Never write 'unclear', 'vague', 'ambiguous', or 'I can't tell'. If the prompt is thin, infer the single most reasonable concrete task and name it plainly. You are the expert — commit.\n\
        - Preserve the user's intent and voice. Never invent file names, symbols, credentials, IPs, or deadlines — only name things you actually found in the repo. When a detail is unspecified, choose a sensible default (and the obvious stack if none is given) and state it in ONE line inside the task; do not ask.\n\
        - Make it complete and unambiguous, but keep scope to what was asked — do not gold-plate or add unrequested features.\n\
        - NEVER ask the user anything. You rewrite the prompt into a task for the coding agent — you do not interrogate the user and you NEVER output a questions section. Resolve every unknown yourself: pick the sensible default and state it in ONE line inside the task. Never ask the user to re-describe symptoms you can already see in the repo.\n\
        Output exactly these two headings, no surrounding code fences:\n\
        Interpretation: ONE short factual sentence naming the concrete task. No reasoning, no 'the prompt is…'.\n\
        Enhanced prompt: the rewritten task as clean GitHub-flavored Markdown — a complete, self-contained spec the agent can execute without further questions. Use a numbered list for sequential steps; state concrete defaults, error/edge-case handling, and a short 'Done when' acceptance check where it sharpens the task. Wrap every file path, function, variable, command, and dependency in backtick `code` spans (e.g. `routes/logo.js`, `createCanvas()`). Do NOT use decorative bold (no ** asterisks). This is the complete deliverable.\n\
        \n\
        SHAPE MAP — FIRST classify the request's SHAPE, THEN refine WITHIN that shape; never convert a non-task into a task and never complete it. Interpretation names the ASK in that shape ('Answer whether…', 'Brainstorm options for…', 'Compare…', 'Plan…'), never 'unclear'. (a) QUESTION ('what/why/how/which/can-we/should-we…?') -> a sharper, scoped QUESTION for the agent to investigate and answer: state what a complete answer must cover and which real files/areas to ground it in — do NOT answer it. (b) BRAINSTORM / IDEATION ('ideas for…', 'what could we add', 'what should I build') -> a tighter IDEATION BRIEF: goal, constraints, how many and what kind of options, and the selection criteria — do NOT list the ideas. (c) PLAN / DESIGN ('plan…', 'how would we approach…') -> a PLANNING ASK naming what the plan must decide and the constraints it must respect — do NOT write the plan. (d) COMPARE / RESEARCH / ADVISE ('X vs Y', 'which is better', 'is this good', 'review…') -> a COMPARISON/DECISION ASK naming the options, the evaluation axes, and the deliverable (recommendation with trade-offs and the single condition that flips it) — do NOT pick the winner. (e) BUILD / FIX / REFACTOR / MIGRATE -> the ready-to-run coding TASK with numbered steps and a 'Done when' check. When the shape is genuinely unclear, default to the implied build TASK. In EVERY shape: state sensible defaults inline, never ask the USER anything, keep the two-heading contract, and refine the ask — never fulfill it.\n\
        \n\
        WORKED EXAMPLES (study the shape and rigor; never reuse their content):\n\
        \n\
        1) BUG FIX —\n\
        USER: the search box lags badly when i type\n\
        Interpretation: Fix the input lag in the search box while typing.\n\
        Enhanced prompt:\n\
        Typing in the search box is janky. Find and fix the cause:\n\
        1. Locate the search input handler (`grep` the search component) and check whether it filters or queries synchronously on every keystroke.\n\
        2. Debounce the query (~200ms) and move filtering off the keystroke path; memoise the filtered list.\n\
        3. Confirm large result sets aren't re-rendering the whole list per keystroke.\n\
        Done when: typing stays smooth on a large dataset and results still update correctly.\n\
        \n\
        2) FEATURE —\n\
        USER: add a dark mode toggle\n\
        Interpretation: Add a user-toggleable, persisted dark mode.\n\
        Enhanced prompt:\n\
        Add a dark-mode toggle that persists across reloads.\n\
        1. Add a theme toggle to the top bar/settings.\n\
        2. Drive theming off the existing CSS variable/token system; add a dark palette using current tokens (no new hardcoded colors).\n\
        3. Persist the choice in `localStorage` and apply it before first paint to avoid a flash.\n\
        Done when: toggling switches the whole UI and the choice survives a reload.\n\
        \n\
        3) REFACTOR —\n\
        USER: the auth file is a mess, clean it up\n\
        Interpretation: Refactor the auth module for clarity without changing behaviour.\n\
        Enhanced prompt:\n\
        Refactor the auth module (`read_file` it first) to cut complexity while preserving behaviour.\n\
        1. Identify the distinct responsibilities currently mixed together (session, token, middleware).\n\
        2. Split them into focused, well-named units; remove dead code and duplication.\n\
        3. Keep the public API and behaviour identical; do not change auth logic or add features.\n\
        Done when: the module is split cleanly and existing auth tests still pass.\n\
        \n\
        4) PERF / MIGRATION —\n\
        USER: switch our http-over-ssh baseline to openssh local forwarding instead of raw sessions\n\
        Interpretation: Switch the HTTP-over-SSH baseline test to OpenSSH local forwarding (`ssh -L`) instead of programmatic tunnels.\n\
        Enhanced prompt:\n\
        Change the baseline to use OpenSSH local forwarding (`ssh -L`) instead of the programmatic tunnel built with `SshSession::createForwardL()`, so it reflects real-world SSH tunneling.\n\
        1. Spawn an external `ssh -L <local>:<host>:<port>` process and wait until the local port accepts connections.\n\
        2. Run the existing HTTP perf suite against the forwarded port; tear the process down after (success or failure).\n\
        3. Keep the programmatic-tunnel path available so the two can be compared head-to-head.\n\
        Done when: the baseline runs over `ssh -L` and its numbers compare directly against the programmatic tunnel.\n\
        \n\
        5) QUESTION (mirror into a sharper question, never the answer) —\n\
        USER: what features could we add to the agent upload flow?\n\
        Interpretation: Answer what features could extend the agent file-upload flow, grounded in the current implementation.\n\
        Enhanced prompt:\n\
        This is a QUESTION to answer, not a task to build. Investigate the existing agent file-upload flow and answer what features could meaningfully extend it — do NOT implement anything or pick one to build.\n\
        1. Read the real flow first: `backend/src/sftp/mod.rs` (upload, download, listdir), `backend/src/agent_detect/mod.rs` (running-agent detection), and the UI entry points `frontend/src/components/SftpPage.tsx` and `frontend/src/components/TerminalView.tsx`.\n\
        2. Inventory what exists today so proposals extend it rather than duplicate it.\n\
        3. For each candidate feature give: the concrete user need, where in those files it would hook in, rough size (S/M/L), and any conflict with the project's constraints (local-only, no telemetry, no new deps without review).\n\
        4. Rank by value-to-effort and name the single highest-leverage one.\n\
        Done when: the answer is a grounded, prioritized shortlist citing the real files each idea touches — a recommendation in prose, not code.\n\
        \n\
        6) BRAINSTORM (mirror into an ideation brief, never the ideas) —\n\
        USER: brainstorm approaches for reworking how we detect which agent is running in a remote session\n\
        Interpretation: Brainstorm and compare approaches for reworking remote AI-agent detection.\n\
        Enhanced prompt:\n\
        This is a BRAINSTORM to explore, not a task to build. Read the current detection first, then propose and compare candidate approaches — do NOT pick one or implement it.\n\
        1. Ground in the real code: read `backend/src/agent_detect/mod.rs` and the session layer it hooks into.\n\
        2. Summarize how detection works today and where it can be wrong, so alternatives anchor to the real baseline.\n\
        3. Propose 4-6 distinct approaches (e.g. process-tree inspection, PTY/prompt heuristics, an explicit user override). For each: the signal it relies on, failure modes, which files/symbols it touches, and rough effort.\n\
        4. End with the open questions a human must resolve.\n\
        Done when: the output is a comparison-ready set of options with trade-offs and a suggested first pick — no chosen direction and no implementation.\n\
        \n\
        NEVER DO THIS (bad output) —\n\
        Interpretation: Unclear / the prompt is vague.  ← WRONG: always NAME the task.\n\
        Enhanced prompt: Sure, here's how — <answer or code>…  ← WRONG: you REWRITE the prompt, you never solve or answer it; never ask the user questions; never invent files you didn't find; never add unrequested features (auth, tests, rate-limiting nobody asked for).\n\
        Interpretation: Features to add are X, Y, Z. / Enhanced prompt: Add 1) resumable transfers, 2) a transfer queue, 3) checksum verification…  ← WRONG: that ANSWERS the question and LISTS the ideas. A question/brainstorm/plan/compare must be MIRRORED into a sharper QUESTION, ideation BRIEF, planning ASK, or comparison ASK for the agent — you never answer it, list the ideas, write the plan, pick the winner, or output the deliverable yourself."
    } else {
        "You are Tersh Prompt Enhancer — the sharpest prompt rewriter there is. You REWRITE the user's rough prompt into a sharper version of the SAME request for a SEPARATE coding agent — usually a ready-to-run coding task, but you MIRROR the request's shape (see the first rule). You are NOT that agent and NOT a chat assistant: you never answer or do the work — you produce a better PROMPT, and you are not talking to the user.\n\
        Hard rules:\n\
        - MIRROR THE REQUEST'S SHAPE, SHARPEN IT, NEVER FULFILL IT: your output is always a sharper version of the SAME ask the user made, handed to the coding agent — a question becomes a tighter question, a brainstorm becomes a tighter idea-generation brief, a plan/compare/advise becomes a rigorous planning/comparison/decision ask, a build/fix becomes a ready-to-run task. You sharpen WHAT IS ASKED and HOW A GOOD ANSWER IS JUDGED (scope, criteria, what it must cover); you NEVER supply the answer, the idea list, the plan, the verdict, or the code yourself. If your draft contains the deliverable instead of the refined ask for it, you have failed. (See SHAPE MAP below.)\n\
        - THINK FIRST about the user's TRUE goal and what would DEFEAT it, before writing. If they want to BUILD or MAKE a capability, IMPLEMENT it — never just call or proxy the third-party service that already provides exactly that (building a logo fetcher is NOT 'call Clearbit'; building auth is NOT 'use Auth0'). Favor built-in/native APIs and durable approaches over extra dependencies or deprecated/paid services. Reason silently; output only the two headings.\n\
        - INTERPRETATION ALWAYS NAMES A CONCRETE TASK. Never write 'unclear', 'vague', or 'ambiguous'. If the prompt is thin, infer the single most reasonable concrete task and name it plainly. You are the expert — commit.\n\
        - Preserve the user's intent and voice. Do not invent file names, requirements, credentials, IPs, or deadlines. This is a fresh-build / general request: keep it self-contained, pick the obvious stack and sensible defaults when unspecified, and state each in ONE line inside the task. Cover error/edge-case handling. Keep scope to what was asked — no gold-plating.\n\
        - NEVER ask the user anything. You rewrite the prompt into a task for the coding agent — you do not interrogate the user and you NEVER output a questions section. Resolve every unknown yourself: pick the obvious stack and sensible defaults and state each in ONE line inside the task.\n\
        Output exactly these two headings, no surrounding code fences:\n\
        Interpretation: ONE short factual sentence naming the concrete task. No reasoning, no 'the prompt is…'.\n\
        Enhanced prompt: the rewritten task as clean GitHub-flavored Markdown — a complete, self-contained spec the agent can execute without further questions. Use numbered lists for sequential steps; state concrete defaults, the chosen stack, error/edge-case handling, and a short 'Done when' acceptance check where it sharpens the task. Wrap file paths, identifiers, commands, and dependencies in backtick `code` spans; do NOT use decorative bold (no ** asterisks). The complete deliverable.\n\
        \n\
        SHAPE MAP — FIRST classify the request's SHAPE, THEN refine WITHIN that shape; never convert a non-task into a task and never complete it. Interpretation names the ASK in that shape ('Answer whether…', 'Brainstorm options for…', 'Compare…', 'Plan…'), never 'unclear'. (a) QUESTION ('what/why/how/which/can-we/should-we…?') -> a sharper, scoped QUESTION for the agent to answer: state what a complete answer must cover and the assumed context — do NOT answer it. (b) BRAINSTORM / IDEATION ('ideas for…', 'what should I build') -> a tighter IDEATION BRIEF: goal, constraints, how many and what kind of options, and the selection criteria — do NOT list the ideas. (c) PLAN / DESIGN ('plan…', 'how would we approach…') -> a PLANNING ASK naming what the plan must decide and the constraints — do NOT write the plan. (d) COMPARE / RESEARCH / ADVISE ('X vs Y', 'which is better') -> a COMPARISON/DECISION ASK naming the options, the evaluation axes, and the deliverable (recommendation with trade-offs and the single condition that flips it) — do NOT pick the winner. (e) BUILD / FIX -> the ready-to-run coding TASK with numbered steps and a 'Done when' check. When the shape is genuinely unclear, default to the implied build TASK. In EVERY shape: state sensible defaults inline, never ask the USER anything, keep the two-heading contract, and refine the ask — never fulfill it.\n\
        \n\
        WORKED EXAMPLES (study the shape and rigor; never reuse their content):\n\
        \n\
        1) WEB SERVICE —\n\
        USER: build me a free screenshot api service\n\
        Interpretation: Build a free, self-hosted Node.js screenshot API using headless Chromium.\n\
        Enhanced prompt:\n\
        Build a free, self-hosted screenshot API in Node.js (`express` + `puppeteer`, both MIT) exposing `GET /screenshot?url=…` that returns a PNG or JPEG.\n\
        1. Init the project (`npm init -y`, set `\"type\": \"module\"`); install `express` and `puppeteer`.\n\
        2. Launch ONE shared `puppeteer` browser at startup with `--no-sandbox`; reuse it across requests (never per-request).\n\
        3. `GET /screenshot` params: `url` (required, http/https only), `width` (1280), `height` (800), `fullPage` (false), `format` (png), `quality` (80). Return 400 on missing/invalid `url`.\n\
        4. Capture: new page, set viewport, navigate with `waitUntil: 'networkidle0'` and a 30s timeout, `page.screenshot()`, close the page in a `finally`.\n\
        5. Set the right `Content-Type`; 500 `{ error }` on failure; add `GET /health`; close the browser on `SIGINT`/`SIGTERM`.\n\
        Done when: `curl 'localhost:3000/screenshot?url=https://example.com' -o out.png` writes a valid PNG.\n\
        \n\
        2) CLI TOOL —\n\
        USER: make me a cli to bulk-rename files\n\
        Interpretation: Build a CLI that batch-renames files by pattern.\n\
        Enhanced prompt:\n\
        Build a small Node.js CLI (`bulk-rename`) that renames files in a directory by find/replace or a numbering template.\n\
        1. Parse args: a target dir/glob, a `--find`/`--replace` pair or a `--template` like `photo-{n}`, and `--apply` (default is dry-run).\n\
        2. Resolve matches, compute target names, and detect/skip collisions.\n\
        3. Dry-run prints the planned renames; only mutate the filesystem when `--apply` is passed.\n\
        Done when: a dry-run prints the plan and `--apply` performs the renames safely with no collisions.\n\
        \n\
        3) BOT —\n\
        USER: build a discord bot that posts the daily weather\n\
        Interpretation: Build a Discord bot that posts a daily weather summary to a channel.\n\
        Enhanced prompt:\n\
        Build a Discord bot (`discord.js`) that posts a daily weather summary for a configured location.\n\
        1. Read config from env: `DISCORD_TOKEN`, `CHANNEL_ID`, `LOCATION` — never hardcode the token.\n\
        2. Fetch weather from a free API (e.g. Open-Meteo, no key required) and format a short summary.\n\
        3. Post on a daily schedule at a configurable hour, plus an on-demand `!weather` command.\n\
        Done when: the bot posts the summary on schedule and replies to `!weather`.\n\
        \n\
        4) VAGUE GREENFIELD (resolve unknowns yourself, never ask) —\n\
        USER: build me an app to track my gym workouts\n\
        Interpretation: Build a simple workout-tracking app (log workouts, view history).\n\
        Enhanced prompt:\n\
        Build a minimal, local-first workout tracker as a web app (`React` + `localStorage`, no backend, single user).\n\
        1. Log a workout: date, exercise, sets, reps, weight.\n\
        2. List past workouts with per-exercise history.\n\
        3. Persist locally; no accounts.\n\
        Done when: a user can log a workout and see it in their history after a reload.\n\
        \n\
        5) QUESTION (mirror into a scoped question, never the answer) —\n\
        USER: is sqlite or postgres better for a small self-hosted note app?\n\
        Interpretation: Answer whether SQLite or Postgres better fits a small self-hosted note app, with the trade-offs.\n\
        Enhanced prompt:\n\
        This is a QUESTION to answer, not an app to build. Compare `SQLite` and `PostgreSQL` for a small, single-user, self-hosted note app and recommend one — do NOT scaffold a project or write app code.\n\
        1. State the assumed context in one line (single user, local-first, modest data volume, simple deploy) and note any assumption you change.\n\
        2. Compare both on the criteria that matter here: operational/setup burden, on-disk footprint, single-user concurrency, backup/portability, and the migration path if it grows.\n\
        3. For each criterion say which option wins and why, in a sentence or two.\n\
        Conclude with a clear recommendation for THIS context and the single condition under which the other choice would win. Respond in prose with a short trade-off table — no schema or connection code.\n\
        \n\
        6) BRAINSTORM (mirror into an idea brief, never the ideas) —\n\
        USER: what should i build for a weekend project with the openai api\n\
        Interpretation: Brainstorm weekend-sized project ideas built on the OpenAI API.\n\
        Enhanced prompt:\n\
        This is a BRAINSTORM to explore, not a task to build. Generate weekend-sized project ideas that use the OpenAI API — do NOT start building any of them or write code.\n\
        1. State the assumed constraints in one line (solo build, ~2 days, one core feature, runnable locally) and apply them to every idea.\n\
        2. Produce 5-7 distinct ideas spanning different shapes (a CLI, a small web app, a bot, an automation, a data tool).\n\
        3. For each idea give: a one-sentence concept, the single core OpenAI capability it leans on, the rough stack, and why it fits a weekend.\n\
        Rank by impact-to-effort and end with a one-line pick of where to start. Deliver the idea menu only — not a chosen project and not its code.\n\
        \n\
        NEVER DO THIS (bad output) —\n\
        Interpretation: Unclear / the prompt is vague.  ← WRONG: always NAME a concrete MVP.\n\
        Enhanced prompt: A screenshot API is a service that captures web pages; here's some code… or: what database do you want to use?  ← WRONG: you REWRITE the prompt, you never explain, solve, or ask the user anything; don't bolt on auth, databases, rate-limiting, or tests nobody asked for.\n\
        Interpretation: Here are 5 weekend project ideas: 1) a chatbot, 2) a summarizer…  ← WRONG: that ANSWERS the brainstorm and LISTS the ideas. A question/brainstorm/plan/compare must be MIRRORED into a sharper QUESTION, ideation BRIEF, planning ASK, or comparison ASK for the agent — you never answer it, list the ideas, write the plan, or pick the winner yourself."
    }
}

struct PromptUserMessage {
    message: String,
    context_trace: Vec<PromptContextTraceItem>,
}

async fn build_user_message(
    prompt: &str,
    scope: Option<&crate::brain::BrainScope>,
    brain_enabled: bool,
    context_decision: &crate::brain::index::PromptContextDecision,
    has_project_index: bool,
    embedding: Option<&crate::brain::embed::EmbeddingConfig>,
) -> PromptUserMessage {
    // Keep the note internally consistent with the ACTUAL decision (brain_enabled).
    // Don't echo context_decision.reason when the index is in use — its keyword
    // reason can say "no context needed" while we're using it, which contradicts
    // and misleads the model.
    let decision_note = if brain_enabled {
        format!(
            "Tersh context decision: intent={:?}, project_index_available={}, index_in_use=true. Ground the rewrite in the project context below and your read-only tools.\n\n",
            context_decision.kind, has_project_index
        )
    } else {
        format!(
            "Tersh context decision: intent={:?}, project_index_available={}, index_in_use=false, reason={}\n\n",
            context_decision.kind, has_project_index, context_decision.reason
        )
    };
    if brain_enabled {
        if let Some(scope) = scope {
            let id = crate::brain::BrainId::from_scope_key(&scope.scope_key());
            if let Ok(Some(context)) =
                crate::brain::index::retrieve_context_with_trace(&id, prompt, embedding).await
            {
                let context_trace = context
                    .trace
                    .into_iter()
                    .map(|item| PromptContextTraceItem {
                        tool: item.tool,
                        target: item.target,
                        status: item.status,
                    })
                    .collect();
                return PromptUserMessage {
                    message: format!(
                        "{decision_note}Project index context, use only if helpful:\n{}\n\nUser prompt:\n{}",
                        context.text,
                        prompt.trim()
                    ),
                    context_trace,
                };
            }
        }
        PromptUserMessage {
            message: format!("{decision_note}User prompt:\n{}", prompt.trim()),
            context_trace: Vec::new(),
        }
    } else {
        PromptUserMessage {
            message: format!("{decision_note}User prompt:\n{}", prompt.trim()),
            context_trace: Vec::new(),
        }
    }
}

async fn detect_project_scope(
    state: &State<'_, AppState>,
    req: &PromptEnhanceRequest,
) -> AppResult<Option<crate::brain::BrainScope>> {
    if let Some(brain_id) = req
        .brain_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let id = crate::brain::BrainId(brain_id.to_string());
        if let Some(handle) = state.brain.get(&id).await {
            return Ok(Some(handle.meta.lock().await.scope.clone()));
        }
        return Err(AppError::Invalid(
            "selected project index is unavailable; choose or rebuild the project index".into(),
        ));
    }
    // Prompt enhancement project context is selected-project only. Do not infer
    // from terminal cwd, SSH session, or host: a user can have many unrelated
    // projects open in one terminal/VPS, and wrong context is worse than no
    // context.
    Ok(None)
}

async fn refresh_prompt_scope_index_if_expired(
    state: &State<'_, AppState>,
    scope: &crate::brain::BrainScope,
    session_id: Option<&str>,
) -> AppResult<bool> {
    let id = crate::brain::BrainId::from_scope_key(&scope.scope_key());
    if !crate::brain::index::is_expired(&id, crate::brain::index::AUTO_REFRESH_AFTER_SECS).await? {
        return Ok(false);
    }
    match scope.clone() {
        crate::brain::BrainScope::Local { .. } => state.brain.refresh_if_expired(scope).await,
        crate::brain::BrainScope::Remote { remote_root, .. } => {
            let session_id = session_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::Invalid(
                        "active SSH session required to refresh remote project index".into(),
                    )
                })?;
            let session = state.sessions.get(session_id).await?;
            // Shared guard: skip if any refresh path is already rebuilding this
            // brain (the reconnect resync, the manual button, or another tab).
            // The guard releases on drop — including an unwind — so a panic in
            // build/persist can't wedge future refreshes.
            let Some(_guard) = state.brain.begin_refresh(&id) else {
                return Ok(false);
            };
            let meta = crate::brain::BrainMeta::new(scope.clone());
            let session_persist = session.clone();
            // Silent, expiry-driven refresh — no UI subscriber, so no progress.
            let index =
                crate::brain::index::build_remote(meta, session, remote_root, None, None).await?;
            // Remote brains persist to the VPS (<root>/.tersh), not the Mac.
            let persist_meta = crate::brain::BrainMeta::new(scope.clone());
            crate::brain::index::persist_remote(&session_persist, &persist_meta, index).await?;
            Ok(true)
        }
    }
}

#[tauri::command]
pub async fn set_key_passphrase(key_id: String, passphrase: String) -> AppResult<()> {
    let entry = keyring::Entry::new(KEYPASS_SERVICE, &key_id)
        .map_err(|e| AppError::Internal(format!("keychain entry: {e}")))?;
    entry
        .set_password(&passphrase)
        .map_err(|e| AppError::Internal(format!("keychain set: {e}")))?;
    Ok(())
}

#[tauri::command]
pub async fn clear_key_passphrase(key_id: String) -> AppResult<()> {
    let entry = keyring::Entry::new(KEYPASS_SERVICE, &key_id)
        .map_err(|e| AppError::Internal(format!("keychain entry: {e}")))?;
    let _ = entry.delete_credential();
    Ok(())
}

/// Returns the subset of key_ids for which a passphrase is stored in keychain.
#[tauri::command]
pub async fn list_active_keypass_keys(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    let keys = {
        let vault = state.vault.lock().await;
        vault.list_keys()?
    };
    let mut active = Vec::new();
    for k in keys {
        let entry = match keyring::Entry::new(KEYPASS_SERVICE, &k.id) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.get_password().is_ok() {
            active.push(k.id);
        }
    }
    Ok(active)
}

#[tauri::command]
pub async fn detect_remote_agent(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<Option<AgentKind>> {
    let session = state.sessions.get(&session_id).await?;
    Ok(agent_detect::detect(&session).await?.map(|(k, _)| k))
}

#[tauri::command]
pub async fn detect_remote_os(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<Option<String>> {
    let session = state.sessions.get(&session_id).await?;
    let out = session
        .exec_oneshot(
            "cat /etc/os-release 2>/dev/null || uname -s 2>/dev/null",
            16 * 1024,
        )
        .await?;
    let text = String::from_utf8_lossy(&out).to_ascii_lowercase();
    let os = if text.contains("ubuntu") {
        Some("ubuntu")
    } else if text.contains("debian") {
        Some("debian")
    } else if text.contains("fedora") {
        Some("fedora")
    } else if text.contains("arch") {
        Some("arch")
    } else if text.contains("alpine") {
        Some("alpine")
    } else if text.contains("centos") {
        Some("centos")
    } else if text.contains("red hat") || text.contains("rhel") {
        Some("rhel")
    } else if text.contains("darwin") {
        Some("apple")
    } else if text.contains("freebsd") || text.contains("openbsd") || text.contains("netbsd") {
        Some("bsd")
    } else if text.contains("linux") {
        Some("linux")
    } else {
        None
    };
    Ok(os.map(str::to_string))
}

// ════════════════════════════════════════════════════════════════════════════
// KEYCHAIN
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_keys(state: State<'_, AppState>) -> AppResult<Vec<KeyRow>> {
    let vault = state.vault.lock().await;
    vault.list_keys()
}

#[tauri::command]
pub async fn delete_key(state: State<'_, AppState>, id: String) -> AppResult<()> {
    {
        let vault = state.vault.lock().await;
        vault.delete_key(&id)?;
    }
    let legacy_keypass_service = legacy_keypass_service();
    for service in [KEYPASS_SERVICE, legacy_keypass_service.as_str()] {
        if let Ok(entry) = keyring::Entry::new(service, &id) {
            let _ = entry.delete_credential();
        }
    }
    Ok(())
}

/// Generate an ed25519 keypair on disk under ~/.ssh and record in vault.
/// Returns the public key OpenSSH-format string + fingerprint.
#[derive(Deserialize)]
pub struct GenerateKeyInput {
    pub label: String,
    pub comment: Option<String>,
}

#[derive(Serialize)]
pub struct GeneratedKey {
    pub id: String,
    pub public_key: String,
    pub fingerprint: String,
    pub private_path: String,
}

#[tauri::command]
pub async fn generate_key(
    state: State<'_, AppState>,
    input: GenerateKeyInput,
) -> AppResult<GeneratedKey> {
    let comment = input
        .comment
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| input.label.clone());

    let home =
        user_home_dir().ok_or_else(|| AppError::Internal("home directory unavailable".into()))?;
    let ssh_dir = home.join(".ssh");
    tokio::fs::create_dir_all(&ssh_dir).await?;
    let safe = input
        .label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut private_path = ssh_dir.join(format!("id_ed25519_{}", safe));
    if private_path.exists() || private_path.with_extension("pub").exists() {
        private_path = ssh_dir.join(format!(
            "id_ed25519_{}_{}",
            safe,
            uuid::Uuid::new_v4().simple()
        ));
    }
    let pub_path = private_path.with_extension("pub");

    // Use the OS's ssh-keygen — available on every modern Mac/Linux.
    let status = tokio::process::Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(comment)
        .arg("-f")
        .arg(&private_path)
        .status()
        .await
        .map_err(|e| AppError::Internal(format!("spawn ssh-keygen: {e}")))?;
    if !status.success() {
        return Err(AppError::Internal(format!(
            "ssh-keygen exited with status {status}"
        )));
    }

    let pub_openssh = tokio::fs::read_to_string(&pub_path)
        .await
        .map_err(|e| AppError::Internal(format!("read pub key: {e}")))?
        .trim()
        .to_string();

    // Fingerprint via ssh-keygen -lf
    let fp_out = tokio::process::Command::new("ssh-keygen")
        .arg("-lf")
        .arg(&pub_path)
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("ssh-keygen -lf: {e}")))?;
    let fp_text = String::from_utf8_lossy(&fp_out.stdout);
    let fingerprint = fp_text
        .split_whitespace()
        .nth(1)
        .unwrap_or("SHA256:unknown")
        .to_string();

    let vault = state.vault.lock().await;
    let id = vault.add_key(AddKeyInput {
        label: input.label,
        kind: "ed25519".into(),
        public_key: pub_openssh.clone(),
        fingerprint: fingerprint.clone(),
        private_path: Some(private_path.to_string_lossy().into_owned()),
    })?;

    Ok(GeneratedKey {
        id,
        public_key: pub_openssh,
        fingerprint,
        private_path: private_path.to_string_lossy().into_owned(),
    })
}

/// Import an existing key from a file picker path.
#[derive(Deserialize)]
pub struct ImportKeyInput {
    pub label: String,
    pub path: String,
}

#[tauri::command]
pub async fn import_key(state: State<'_, AppState>, input: ImportKeyInput) -> AppResult<KeyRow> {
    // Resolve the public key path (siblings to the private key path)
    let p = std::path::Path::new(&input.path);
    validate_private_key_import_path(p)?;
    let mut pub_path = p.to_path_buf();
    let name = format!(
        "{}.pub",
        p.file_name().and_then(|n| n.to_str()).unwrap_or("key")
    );
    pub_path.set_file_name(name);

    let public_key = tokio::fs::read_to_string(&pub_path)
        .await
        .map_err(|e| AppError::Invalid(format!("read public key {}: {e}", pub_path.display())))?
        .trim()
        .to_string();

    let kind = if public_key.starts_with("ssh-ed25519") {
        "ed25519"
    } else if public_key.starts_with("ssh-rsa") {
        "rsa"
    } else if public_key.starts_with("ecdsa-") {
        "ecdsa"
    } else {
        "unknown"
    }
    .to_string();

    // Real SHA256 fingerprint via ssh-keygen -lf (system binary)
    let fp_out = tokio::process::Command::new("ssh-keygen")
        .arg("-lf")
        .arg(&pub_path)
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("ssh-keygen -lf: {e}")))?;
    let fp_text = String::from_utf8_lossy(&fp_out.stdout);
    let fingerprint = fp_text
        .split_whitespace()
        .nth(1)
        .unwrap_or("SHA256:unknown")
        .to_string();

    let vault = state.vault.lock().await;
    let id = vault.add_key(AddKeyInput {
        label: input.label.clone(),
        kind: kind.clone(),
        public_key: public_key.clone(),
        fingerprint: fingerprint.clone(),
        private_path: Some(input.path.clone()),
    })?;
    Ok(KeyRow {
        id,
        label: input.label,
        kind,
        public_key,
        fingerprint,
        private_path: Some(input.path),
        created_at: 0,
    })
}

fn validate_private_key_import_path(path: &std::path::Path) -> AppResult<()> {
    if !path.is_absolute() {
        return Err(AppError::Invalid("key import path must be absolute".into()));
    }
    if path_has_parent_component(path) {
        return Err(AppError::Invalid(
            "key import path cannot contain '..' components".into(),
        ));
    }
    if path.parent().is_none() {
        return Err(AppError::Invalid(
            "key import path cannot be filesystem root".into(),
        ));
    }
    if path.extension().and_then(|s| s.to_str()) == Some("pub") {
        return Err(AppError::Invalid(
            "select the private key file, not the .pub file".into(),
        ));
    }
    if let Ok(home) = std::env::var("HOME") {
        if path == std::path::Path::new(&home) {
            return Err(AppError::Invalid(
                "key import path cannot be the home directory".into(),
            ));
        }
    }
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::Invalid(format!("private key file does not exist: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Invalid("key import path must be a file".into()));
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════════
// SNIPPETS
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_snippets(state: State<'_, AppState>) -> AppResult<Vec<SnippetRow>> {
    let vault = state.vault.lock().await;
    vault.list_snippets()
}

#[tauri::command]
pub async fn add_snippet(state: State<'_, AppState>, input: AddSnippetInput) -> AppResult<String> {
    validate_snippet_input(&input)?;
    let vault = state.vault.lock().await;
    vault.add_snippet(input)
}

#[tauri::command]
pub async fn update_snippet(
    state: State<'_, AppState>,
    id: String,
    input: AddSnippetInput,
) -> AppResult<()> {
    validate_snippet_input(&input)?;
    let vault = state.vault.lock().await;
    vault.update_snippet(&id, input)
}

#[tauri::command]
pub async fn delete_snippet(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let vault = state.vault.lock().await;
    vault.delete_snippet(&id)
}

/// Run a snippet on the active session — types into the prompt buffer (no auto-Enter).
#[tauri::command]
pub async fn run_snippet(
    state: State<'_, AppState>,
    session_id: String,
    snippet_id: String,
) -> AppResult<()> {
    let cmd = {
        let vault = state.vault.lock().await;
        vault
            .list_snippets()?
            .into_iter()
            .find(|s| s.id == snippet_id)
            .ok_or_else(|| AppError::Invalid("snippet not found".into()))?
            .command
    };
    validate_terminal_input_len(cmd.len())?;
    send_to_terminal_session(&state, &session_id, cmd.into_bytes()).await
}

const MAX_SNIPPET_LABEL_CHARS: usize = 120;
const MAX_SNIPPET_COMMAND_BYTES: usize = 128 * 1024;
const MAX_SNIPPET_META_CHARS: usize = 512;

fn validate_snippet_input(input: &AddSnippetInput) -> AppResult<()> {
    if input.label.trim().is_empty() || input.command.trim().is_empty() {
        return Err(AppError::Invalid("label and command are required".into()));
    }
    if input.label.chars().count() > MAX_SNIPPET_LABEL_CHARS {
        return Err(AppError::Invalid(format!(
            "snippet label must be {MAX_SNIPPET_LABEL_CHARS} characters or fewer"
        )));
    }
    if input.command.len() > MAX_SNIPPET_COMMAND_BYTES {
        return Err(AppError::Invalid(format!(
            "snippet command is too large ({} bytes, max {} bytes)",
            input.command.len(),
            MAX_SNIPPET_COMMAND_BYTES
        )));
    }
    for (field, value) in [
        ("description", input.description.as_deref()),
        ("tags", input.tags.as_deref()),
        ("group_path", input.group_path.as_deref()),
    ] {
        if value.map(|s| s.chars().count()).unwrap_or(0) > MAX_SNIPPET_META_CHARS {
            return Err(AppError::Invalid(format!(
                "snippet {field} must be {MAX_SNIPPET_META_CHARS} characters or fewer"
            )));
        }
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════════
// KNOWN HOSTS
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_known_hosts(state: State<'_, AppState>) -> AppResult<Vec<KnownHostRow>> {
    let vault = state.vault.lock().await;
    vault.list_known_hosts()
}

// ════════════════════════════════════════════════════════════════════════════
// PROJECT BRAIN (explicit selected-project index + scoped read tools)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct BrainEnableLocalRequest {
    pub project_path: String,
    #[serde(default)]
    pub ai: Option<BrainIndexAiConfig>,
}

#[derive(Serialize)]
pub struct BrainEnableResponse {
    pub brain_id: String,
}

#[tauri::command]
pub async fn brain_enable_local(
    state: State<'_, AppState>,
    req: BrainEnableLocalRequest,
) -> AppResult<BrainEnableResponse> {
    let path = std::path::PathBuf::from(&req.project_path);
    if !path.is_absolute() {
        return Err(AppError::Invalid("project path must be absolute".into()));
    }
    let canonical = std::fs::canonicalize(&path)
        .map_err(|e| AppError::Invalid(format!("project root unavailable: {e}")))?;
    if !canonical.is_dir() {
        return Err(AppError::Invalid("brain root must be a directory".into()));
    }
    let scope = crate::brain::BrainScope::Local {
        root: canonical.clone(),
    };
    let meta = crate::brain::BrainMeta::new(scope.clone());
    let embed = req.ai.as_ref().and_then(|a| a.to_embedding_config());
    if let Some(ref e) = embed {
        e.validate()?;
    }
    let mut index = crate::brain::index::build_local(meta, canonical, embed.as_ref()).await?;
    if let Some(ai) = req.ai.as_ref() {
        apply_project_digest_synthesis(ai, &mut index).await;
    }
    let id = state.brain.register_scope(scope).await?;
    crate::brain::index::write_index(&index).await?;
    Ok(BrainEnableResponse { brain_id: id.0 })
}

#[derive(Deserialize)]
pub struct BrainEnableRemoteRequest {
    pub session_id: String,
    #[serde(default)]
    pub remote_root: Option<String>,
    #[serde(default)]
    pub ai: Option<BrainIndexAiConfig>,
    /// Renderer-minted id so it can subscribe to `brain://index/<index_id>/progress`
    /// BEFORE invoking — the real brain_id doesn't exist until register_scope at
    /// the end of this command, so live progress is keyed on this instead.
    #[serde(default)]
    pub index_id: Option<String>,
}

#[tauri::command]
pub async fn brain_enable_remote(
    app: AppHandle,
    state: State<'_, AppState>,
    req: BrainEnableRemoteRequest,
) -> AppResult<BrainEnableResponse> {
    let session = state.sessions.get(&req.session_id).await?;
    let host_id = state.sessions.host_id_for_session(&req.session_id).await?;
    let fingerprints = {
        let vault = state.vault.lock().await;
        vault.known_fingerprints_for(&host_id)?
    };
    let fingerprint = fingerprints
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Invalid("no known host-key fingerprint for this host".into()))?;

    let supplied = req
        .remote_root
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let remote_root = match supplied {
        Some(r) if r.starts_with('/') => r.to_string(),
        Some(_) => {
            return Err(AppError::Invalid(
                "remote root must be an absolute path".into(),
            ));
        }
        None => match detect_remote_project_root(&session).await? {
            Some(root) => root,
            None => detect_single_remote_project_root(&session)
                .await?
                .ok_or_else(|| {
                AppError::Invalid(
                    "could not auto-detect remote project root; start the remote agent from the project or pick a VPS folder".into(),
                )
                })?,
        },
    };

    let scope = crate::brain::BrainScope::Remote {
        host_id: host_id.clone(),
        host_fingerprint: fingerprint.clone(),
        remote_root: remote_root.clone(),
    };
    let meta = crate::brain::BrainMeta::new(scope.clone());
    let id = meta.id.clone(); // deterministic from host_fingerprint + remote_root
    let embed = req.ai.as_ref().and_then(|a| a.to_embedding_config());
    if let Some(ref e) = embed {
        e.validate()?;
    }
    // Shared in-flight guard (same as refresh/resync/on-use paths): two tabs to
    // the same VPS+root map to ONE brain id and ONE <root>/.tersh store, so we
    // must NOT run two concurrent build+persist. If one's already in flight, just
    // register the scope for this session and return the deterministic id.
    let Some(_guard) = state.brain.begin_refresh(&id) else {
        state.brain.register_scope(scope).await?;
        return Ok(BrainEnableResponse { brain_id: id.0 });
    };
    let result = async {
        // Live progress is keyed on the renderer-supplied index_id.
        let progress = req.index_id.clone().map(|id| (app.clone(), id));
        let session_persist = session.clone();
        let mut index = crate::brain::index::build_remote(
            meta,
            session,
            remote_root.clone(),
            embed.as_ref(),
            progress,
        )
        .await?;
        if let Some(ai) = req.ai.as_ref() {
            apply_project_digest_synthesis(ai, &mut index).await;
        }
        state.brain.register_scope(scope.clone()).await?;
        // Remote brains live on the VPS (<root>/.tersh), not the Mac's disk.
        let persist_meta = crate::brain::BrainMeta::new(scope);
        crate::brain::index::persist_remote(&session_persist, &persist_meta, index).await?;
        Ok::<(), AppError>(())
    }
    .await;
    // _guard releases the slot on drop (here, or on an unwind from a panic above).
    result?;
    Ok(BrainEnableResponse { brain_id: id.0 })
}

async fn detect_remote_project_root(
    session: &std::sync::Arc<crate::ssh::SshSession>,
) -> AppResult<Option<String>> {
    let detected = session.cached_or_detect_agent().await.unwrap_or(None);
    let Some((_, agent_pid)) = detected else {
        return Ok(None);
    };
    let Some(cwd) = agent_detect::agent_cwd(session, agent_pid).await else {
        return Ok(None);
    };
    let trimmed = cwd.trim().trim_end_matches('/');
    // Reject "/" (trims to empty) and non-absolute paths, and never offer a
    // cache/npx/hidden dir as a project — `ps` sees every user's processes, so
    // a root-owned npx-launched agent can report a cwd like /root/.npm/_npx/...
    // This is the same filter the dropdown uses; apply it here too (auto-index).
    if trimmed.is_empty()
        || !trimmed.starts_with('/')
        || agent_detect::is_noise_project_path(trimmed)
    {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

async fn detect_single_remote_project_root(
    session: &std::sync::Arc<crate::ssh::SshSession>,
) -> AppResult<Option<String>> {
    let roots = crate::agent_detect::discover_project_roots(session).await?;
    if roots.len() == 1 {
        Ok(roots.into_iter().next())
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn brain_disable(state: State<'_, AppState>, brain_id: String) -> AppResult<()> {
    let id = crate::brain::BrainId(brain_id);
    // For a remote brain, also delete its store on the VPS (~/.tersh/brain/<id>)
    // if its host is currently connected. Best-effort: if the host is offline we
    // just evict the RAM copy; the VPS files would re-hydrate on next connect.
    let scope = state
        .brain
        .list()
        .await
        .into_iter()
        .find(|b| b.id == id)
        .map(|b| b.scope);
    if let Some(crate::brain::BrainScope::Remote {
        host_id,
        remote_root,
        ..
    }) = scope
    {
        if let Ok(session) = find_active_session_for_host(&state, &host_id).await {
            let dir = crate::brain::index::remote_store_dir(&remote_root);
            let _ = crate::sftp::remove(&session, &format!("{dir}/index.json"), false).await;
            let _ = crate::sftp::remove(&session, &format!("{dir}/meta.json"), false).await;
            let _ = crate::sftp::remove(&session, &dir, true).await;
        }
    }
    state.brain.disable(&id).await
}

#[tauri::command]
/// Returns `true` if it rebuilt, `false` if another refresh on the same brain
/// already held the in-flight guard and this call was a no-op (so the caller can
/// tell "done" from "deferred to an in-flight build").
pub async fn brain_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    brain_id: String,
    ai: Option<BrainIndexAiConfig>,
) -> AppResult<bool> {
    // Capture the id for the progress event BEFORE it's moved into BrainId.
    // Refresh already knows the real brain_id, so live progress keys on it.
    let event_id = brain_id.clone();
    let id = crate::brain::BrainId(brain_id);
    let status = state
        .brain
        .list()
        .await
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| AppError::Invalid("project index not found".into()))?;
    let embed = ai.as_ref().and_then(|a| a.to_embedding_config());
    if let Some(ref e) = embed {
        e.validate()?;
    }
    match status.scope.clone() {
        crate::brain::BrainScope::Local { root } => {
            let meta = crate::brain::BrainMeta::new(status.scope);
            let mut index = crate::brain::index::build_local(meta, root, embed.as_ref()).await?;
            if let Some(ai) = ai.as_ref() {
                apply_project_digest_synthesis(ai, &mut index).await;
            }
            crate::brain::index::write_index(&index).await?;
            Ok(true)
        }
        crate::brain::BrainScope::Remote {
            host_id,
            remote_root,
            ..
        } => {
            let session = find_active_session_for_host(&state, &host_id).await?;
            // Shared guard: don't race a reconnect-resync / on-use refresh on the
            // same brain. If one's already running, treat this as a no-op success.
            // The guard releases on drop, including an unwind from a panic below.
            let Some(_guard) = state.brain.begin_refresh(&id) else {
                return Ok(false);
            };
            let result = async {
                let meta = crate::brain::BrainMeta::new(status.scope.clone());
                let session_persist = session.clone();
                let progress = Some((app.clone(), event_id.clone()));
                let mut index = crate::brain::index::build_remote(
                    meta,
                    session,
                    remote_root,
                    embed.as_ref(),
                    progress,
                )
                .await?;
                if let Some(ai) = ai.as_ref() {
                    apply_project_digest_synthesis(ai, &mut index).await;
                }
                // Remote brains persist to the VPS (<root>/.tersh), not the Mac.
                let persist_meta = crate::brain::BrainMeta::new(status.scope);
                crate::brain::index::persist_remote(&session_persist, &persist_meta, index).await
            }
            .await;
            result.map(|()| true)
        }
    }
}

/// Auto re-sync a remote project's index on reconnect — incremental, gated on
/// the index's age (RECONNECT_REFRESH_AFTER_SECS), bound to the EXACT
/// reconnecting `session_id` (NOT an arbitrary host session, the bug that makes
/// brain_refresh unsafe here), and serialized with every other refresh path via
/// the shared in-flight guard. With `ai`, CHANGED files re-embed; without it,
/// only unchanged-content vectors are kept (carry_over_embeddings) and the
/// stale badge is shown honestly. Returns true if it actually rebuilt.
#[tauri::command]
pub async fn brain_reconnect_resync(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    brain_id: String,
    ai: Option<BrainIndexAiConfig>,
) -> AppResult<bool> {
    let id = crate::brain::BrainId(brain_id);
    let Some(status) = state.brain.list().await.into_iter().find(|b| b.id == id) else {
        return Ok(false);
    };
    let remote_root = match status.scope.clone() {
        crate::brain::BrainScope::Remote { remote_root, .. } => remote_root,
        crate::brain::BrainScope::Local { .. } => return Ok(false),
    };
    // Only re-sync if the index is genuinely stale (cheap age gate, not a
    // per-session flag — survives rapid reconnects with fresh session ids).
    if !crate::brain::index::is_expired(&id, crate::brain::index::RECONNECT_REFRESH_AFTER_SECS)
        .await?
    {
        return Ok(false);
    }
    let embed = ai.as_ref().and_then(|a| a.to_embedding_config());
    if let Some(ref e) = embed {
        e.validate()?;
    }
    // Bind to the EXACT session that reconnected.
    let session = state.sessions.get(&session_id).await?;
    // Serialize against the manual Refresh and the on-use expiry refresh. Guard
    // releases on drop, including an unwind from a panic below.
    let Some(_guard) = state.brain.begin_refresh(&id) else {
        return Ok(false);
    };
    let progress = Some((app.clone(), id.0.clone()));
    let result = async {
        let meta = crate::brain::BrainMeta::new(status.scope.clone());
        let session_persist = session.clone();
        let index =
            crate::brain::index::build_remote(meta, session, remote_root, embed.as_ref(), progress)
                .await?;
        // No AI digest re-synthesis here: it runs on every reconnect and the
        // deterministic digest from build_remote is enough; keep resync cheap.
        let persist_meta = crate::brain::BrainMeta::new(status.scope);
        crate::brain::index::persist_remote(&session_persist, &persist_meta, index).await?;
        Ok::<bool, AppError>(true)
    }
    .await;
    result
}

#[tauri::command]
pub async fn brain_list(state: State<'_, AppState>) -> AppResult<Vec<crate::brain::BrainStatus>> {
    Ok(state.brain.list().await)
}

/// Hydrate the registry with project indexes stored inside the given project
/// folders on THIS VPS — each at `<root>/.tersh/` — pulling meta.json +
/// index.json over SFTP into RAM. Called when a remote connection becomes active
/// (with its candidate project roots) so already-indexed projects show up
/// without re-indexing. Best-effort: a folder with no `.tersh/`, or an
/// unreadable/malformed store, is skipped. Returns the count hydrated.
#[tauri::command]
pub async fn brain_hydrate_remote(
    state: State<'_, AppState>,
    session_id: String,
    roots: Vec<String>,
) -> AppResult<usize> {
    let session = state.sessions.get(&session_id).await?;
    let mut hydrated = 0usize;
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let root = root.trim();
        if !root.starts_with('/') || !seen.insert(root.to_string()) {
            continue;
        }
        let dir = crate::brain::index::remote_store_dir(root);
        let meta_bytes =
            match crate::sftp::read_remote_bytes(&session, &format!("{dir}/meta.json")).await {
                Ok(Some(b)) => b,
                _ => continue,
            };
        let index_bytes =
            match crate::sftp::read_remote_bytes(&session, &format!("{dir}/index.json")).await {
                Ok(Some(b)) => b,
                _ => continue,
            };
        let meta: crate::brain::BrainMeta = match serde_json::from_slice(&meta_bytes) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("hydrate: bad meta in {dir}: {e}");
                continue;
            }
        };
        // Only hydrate remote-scoped brains (a local meta has no business here).
        if !matches!(meta.scope, crate::brain::BrainScope::Remote { .. }) {
            continue;
        }
        let index: crate::brain::index::ProjectIndex = match serde_json::from_slice(&index_bytes) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("hydrate: bad index in {dir}: {e}");
                continue;
            }
        };
        state.brain.hydrate_remote(meta, index).await;
        hydrated += 1;
    }
    if hydrated > 0 {
        tracing::info!("hydrated {hydrated} remote brain(s) from VPS folders");
    }
    Ok(hydrated)
}

/// Discover candidate project roots on the connected VPS, for the Project Index
/// dropdown. The auto-detected agent cwd (if any) is hoisted to the FRONT so it
/// is selectable even when it lives outside `$HOME` (where the scan looks).
#[tauri::command]
pub async fn brain_list_remote_projects(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<Vec<String>> {
    let session = state.sessions.get(&session_id).await?;
    let mut roots = crate::agent_detect::discover_project_roots(&session).await?;
    // Hoist the detected agent cwd to the front so the project you're in shows
    // first — but only if it's a real project dir. `ps` sees every user's
    // processes, so a root-owned `npx`-launched agent can report a cwd like
    // /root/.npm/_npx/<hash>; never offer that cache path as a project.
    if let Ok(Some(cwd)) = detect_remote_project_root(&session).await {
        if !crate::agent_detect::is_noise_project_path(&cwd) {
            roots.retain(|r| r != &cwd);
            roots.insert(0, cwd);
        }
    }
    Ok(roots)
}

#[cfg(test)]
fn validate_known_host_fingerprint(fingerprint: &str) -> AppResult<()> {
    let fp = fingerprint.trim();
    if fp != fingerprint || !fp.starts_with("SHA256:") {
        return Err(AppError::Invalid(
            "fingerprint must start with SHA256:".into(),
        ));
    }
    let body = &fp["SHA256:".len()..];
    if body.len() < 16 || body.len() > 128 {
        return Err(AppError::Invalid("fingerprint length is invalid".into()));
    }
    if !body
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_'))
    {
        return Err(AppError::Invalid(
            "fingerprint contains invalid characters".into(),
        ));
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════════
// TUNNELS (Port Forwarding)
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_tunnels(state: State<'_, AppState>) -> AppResult<Vec<TunnelRow>> {
    let vault = state.vault.lock().await;
    vault.list_tunnels()
}

#[tauri::command]
pub async fn add_tunnel(state: State<'_, AppState>, input: AddTunnelInput) -> AppResult<String> {
    validate_tunnel_input(&input)?;
    let vault = state.vault.lock().await;
    vault.add_tunnel(input)
}

fn validate_tunnel_input(input: &AddTunnelInput) -> AppResult<()> {
    validate_tunnel_parts(
        &input.label,
        &input.kind,
        input.local_port,
        input.remote_host.as_deref(),
        input.remote_port,
    )
}

fn validate_tunnel_parts(
    label: &str,
    kind: &str,
    local_port: i64,
    remote_host: Option<&str>,
    remote_port: Option<i64>,
) -> AppResult<()> {
    if label.trim().is_empty() {
        return Err(AppError::Invalid("label required".into()));
    }
    if !(1..=65_535).contains(&local_port) {
        return Err(AppError::Invalid("local_port out of range".into()));
    }
    match kind {
        "dynamic" => {}
        "local" | "remote" => {
            if remote_host.map(str::trim).unwrap_or("").is_empty() {
                return Err(AppError::Invalid("remote_host required".into()));
            }
            let port = remote_port.unwrap_or(0);
            if !(1..=65_535).contains(&port) {
                return Err(AppError::Invalid("remote_port out of range".into()));
            }
        }
        other => return Err(AppError::Invalid(format!("unknown tunnel kind: {other}"))),
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_tunnel(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.tunnels.stop(&id, &state.sessions).await?;
    let vault = state.vault.lock().await;
    vault.delete_tunnel(&id)
}

// ════════════════════════════════════════════════════════════════════════════
// SESSION LOGS
// ════════════════════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_session_logs(state: State<'_, AppState>) -> AppResult<Vec<SessionLogRow>> {
    let vault = state.vault.lock().await;
    vault.list_session_logs()
}

// ════════════════════════════════════════════════════════════════════════════
// FILE PICKER (for Upload button + key import)
// ════════════════════════════════════════════════════════════════════════════

/// Open a native file picker (multi-select). Returns the chosen absolute
/// paths in order, or an empty vec if cancelled.
///
/// `pick_file` (the legacy singular wrapper below) is kept for callers that
/// only want one path — it picks the first item from the multi result.
#[tauri::command]
pub async fn pick_files(app: AppHandle) -> AppResult<Vec<String>> {
    let Some(paths) = app
        .dialog()
        .file()
        .set_title("Choose files to upload")
        .blocking_pick_files()
    else {
        return Ok(Vec::new());
    };

    paths
        .into_iter()
        .map(|p| {
            p.into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|e| AppError::Internal(format!("selected file path unavailable: {e}")))
        })
        .collect()
}

#[tauri::command]
pub async fn pick_file(app: AppHandle) -> AppResult<Option<String>> {
    let mut paths = pick_files(app).await?;
    Ok(if paths.is_empty() {
        None
    } else {
        Some(paths.remove(0))
    })
}

/// Open a native folder picker. Returns the chosen absolute path or None.
#[tauri::command]
pub async fn pick_folder() -> AppResult<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        let script = r#"set f to POSIX path of (choose folder)
return f"#;
        let output = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .await?;
        if !output.status.success() {
            return Ok(None);
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            Ok(None)
        } else {
            Ok(Some(path))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(AppError::Internal(
            "folder picker only on macOS in v0".into(),
        ))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// SFTP BROWSER
// ════════════════════════════════════════════════════════════════════════════

#[derive(Serialize)]
pub struct SftpListing {
    pub cwd: String,
    pub entries: Vec<crate::sftp::RemoteEntry>,
    /// True when the directory had more than MAX_DIR_ENTRIES — the rest are
    /// hidden. The frontend surfaces a small "showing first N" notice so the
    /// user understands they're not looking at the whole directory.
    pub truncated: bool,
}

#[tauri::command]
pub async fn sftp_list(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> AppResult<SftpListing> {
    let session = state.sessions.get(&session_id).await?;
    let listing = sftp::list_dir(&session, &path).await?;
    Ok(SftpListing {
        cwd: listing.cwd,
        entries: listing.entries,
        truncated: listing.truncated,
    })
}

/// List a directory on the LOCAL filesystem. Used by the SFTP tab's left
/// (Local) pane. No session required.
#[tauri::command]
pub async fn list_local_dir(path: String) -> AppResult<SftpListing> {
    let listing = sftp::list_local_dir(&path).await?;
    Ok(SftpListing {
        cwd: listing.cwd,
        entries: listing.entries,
        truncated: listing.truncated,
    })
}

fn path_has_parent_component(path: &std::path::Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

#[tauri::command]
pub async fn sftp_download(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    remote_path: String,
    local_path: String,
    transfer_id: Option<String>,
) -> AppResult<u64> {
    validate_remote_operation_path(&remote_path)?;
    validate_local_output_file_path(&local_path)?;
    let session = state.sessions.get(&session_id).await?;
    let cancel_flag = if let Some(id) = transfer_id.as_ref() {
        Some(state.transfers.register(id.clone()).await)
    } else {
        None
    };
    let progress = transfer_id.clone().map(|id| (app.clone(), id));
    let result =
        sftp::download_to_path(&session, &remote_path, &local_path, progress, cancel_flag).await;
    if let Some(id) = transfer_id.as_ref() {
        state.transfers.unregister(id).await;
    }
    result
}

#[tauri::command]
pub async fn sftp_upload_to(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_path: String,
    transfer_id: Option<String>,
) -> AppResult<crate::sftp::UploadResult> {
    validate_remote_file_output_path(&remote_path)?;
    let canonical_local_path = canonicalize_upload_source(&local_path).await?;
    let session = state.sessions.get(&session_id).await?;
    let cancel_flag = if let Some(id) = transfer_id.as_ref() {
        Some(state.transfers.register(id.clone()).await)
    } else {
        None
    };
    let progress = transfer_id.clone().map(|id| (app.clone(), id));
    let result = match session.acquire_sftp().await {
        Ok(guard) => {
            sftp::upload_to_path_with_sftp(
                guard.sftp(),
                &canonical_local_path,
                &remote_path,
                progress,
                cancel_flag,
            )
            .await
        }
        Err(e) => Err(e),
    };
    if let Some(id) = transfer_id.as_ref() {
        state.transfers.unregister(id).await;
    }
    result
}

#[tauri::command]
pub async fn sftp_upload_folder_to(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_path: String,
    transfer_id: Option<String>,
) -> AppResult<crate::sftp::UploadFolderResult> {
    validate_remote_operation_path(&remote_path)?;
    let canonical_local_path = canonicalize_upload_folder_source(&local_path).await?;
    let session = state.sessions.get(&session_id).await?;
    let cancel_flag = if let Some(id) = transfer_id.as_ref() {
        Some(state.transfers.register(id.clone()).await)
    } else {
        None
    };
    let progress = transfer_id.clone().map(|id| (app.clone(), id));
    let result = match session.acquire_sftp().await {
        Ok(guard) => {
            sftp::upload_folder_to_dir_with_sftp(
                guard.sftp(),
                &canonical_local_path,
                &remote_path,
                progress,
                cancel_flag,
            )
            .await
        }
        Err(e) => Err(e),
    };
    if let Some(id) = transfer_id.as_ref() {
        state.transfers.unregister(id).await;
    }
    result
}

/// Cancel an in-flight upload/download. The transfer's worker loop will see
/// the flag on its next chunk boundary, shut down its handles cleanly, and
/// return an error. Partial uploads are rolled back (temp file deleted);
/// partial downloads have their local file removed.
#[tauri::command]
pub async fn sftp_cancel_transfer(
    state: State<'_, AppState>,
    transfer_id: String,
) -> AppResult<bool> {
    Ok(state.transfers.cancel(&transfer_id).await)
}

#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, AppState>,
    session_id: String,
    from: String,
    to: String,
) -> AppResult<()> {
    validate_remote_mutation_path(&from)?;
    validate_remote_mutation_path(&to)?;
    let session = state.sessions.get(&session_id).await?;
    sftp::rename(&session, &from, &to).await
}

#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> AppResult<()> {
    validate_remote_mutation_path(&path)?;
    let session = state.sessions.get(&session_id).await?;
    sftp::mkdir(&session, &path).await
}

#[tauri::command]
pub async fn sftp_remove(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
    is_dir: bool,
) -> AppResult<()> {
    validate_remote_mutation_path(&path)?;
    let session = state.sessions.get(&session_id).await?;
    sftp::remove(&session, &path, is_dir).await
}

#[tauri::command]
pub async fn sftp_chmod(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
    mode: u32,
) -> AppResult<()> {
    validate_remote_mutation_path(&path)?;
    validate_remote_chmod_mode(mode)?;
    let session = state.sessions.get(&session_id).await?;
    sftp::chmod(&session, &path, mode).await
}

#[tauri::command]
pub async fn sftp_preview_file(
    state: State<'_, AppState>,
    session_id: String,
    remote_path: String,
) -> AppResult<RemoteFilePreview> {
    validate_remote_operation_path(&remote_path)?;
    let session = state.sessions.get(&session_id).await?;
    let bytes = sftp::read_remote_bytes(&session, &remote_path)
        .await?
        .ok_or_else(|| AppError::Sftp(format!("remote file not found: {remote_path}")))?;
    Ok(RemoteFilePreview {
        path: remote_path,
        bytes,
    })
}

fn validate_remote_operation_path(path: &str) -> AppResult<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err(AppError::Invalid(
            "remote path must be an absolute path".into(),
        ));
    }
    if trimmed.contains('\0') {
        return Err(AppError::Invalid("remote path contains a NUL byte".into()));
    }
    if trimmed != "/" && !trimmed.starts_with('/') && trimmed != "~" && !trimmed.starts_with("~/") {
        return Err(AppError::Invalid(
            "remote path must be absolute or home-relative".into(),
        ));
    }
    if trimmed == "/" {
        return Err(AppError::Invalid(
            "refusing to mutate remote filesystem root".into(),
        ));
    }
    Ok(())
}

fn validate_remote_mutation_path(path: &str) -> AppResult<()> {
    validate_remote_operation_path(path)?;
    if path.trim() == "~" {
        return Err(AppError::Invalid(
            "refusing to mutate remote home directory root".into(),
        ));
    }
    Ok(())
}

fn validate_remote_file_output_path(path: &str) -> AppResult<()> {
    validate_remote_operation_path(path)?;
    let trimmed = path.trim();
    if trimmed == "~" || trimmed.ends_with('/') {
        return Err(AppError::Invalid(
            "remote output path must include a file name".into(),
        ));
    }
    let name = trimmed.rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        return Err(AppError::Invalid(
            "remote output path must include a file name".into(),
        ));
    }
    Ok(())
}

fn validate_remote_chmod_mode(mode: u32) -> AppResult<()> {
    if mode > 0o7777 {
        return Err(AppError::Invalid("chmod mode out of range".into()));
    }
    Ok(())
}

/// Native save dialog (macOS). Returns chosen absolute path or None.
#[tauri::command]
pub async fn save_file_dialog(default_name: Option<String>) -> AppResult<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        let default = default_name.unwrap_or_else(|| "download".into());
        let script = format!(
            "set f to POSIX path of (choose file name default name \"{}\")\nreturn f",
            applescript_string_literal(&default)
        );
        let output = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await?;
        if !output.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            Ok(None)
        } else {
            Ok(Some(s))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = default_name;
        Err(AppError::Internal("save dialog only on macOS in v1".into()))
    }
}

fn applescript_string_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Resolve the default download destination for a remote filename. Returns
/// the absolute path the renderer should hand to `sftp_download`. We drop
/// into the OS-standard $HOME/Downloads folder — no Tersh-specific subdir
/// — so files behave like any browser download. If the target already
/// exists we suffix with " (1)", " (2)", … before the extension.
#[tauri::command]
pub fn default_download_path(remote_filename: String) -> AppResult<String> {
    use std::path::Path;

    // Sanitise: never trust a path from the remote. Take only the basename
    // and drop any directory components / nul bytes. Empty falls back to
    // "download".
    let raw = Path::new(&remote_filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '\0' | '/' | '\\'))
        .collect();
    let basename = if cleaned.trim().is_empty() {
        "download".to_string()
    } else {
        cleaned
    };

    let downloads = user_home_dir()
        .ok_or_else(|| {
            AppError::Internal("home directory unavailable; cannot resolve Downloads".into())
        })?
        .join("Downloads");
    std::fs::create_dir_all(&downloads)
        .map_err(|e| AppError::Internal(format!("could not create ~/Downloads: {e}")))?;

    // Split stem/ext so " (1)" lands BEFORE the extension: foo.tar.gz →
    // foo (1).tar.gz, not foo.tar (1).gz. Use the FIRST dot from the
    // right that isn't a leading dot, so dotfiles (".env") stay intact.
    let (stem, ext) = split_stem_ext(&basename);

    let candidate = downloads.join(&basename);
    if !candidate.exists() {
        return Ok(candidate.to_string_lossy().into_owned());
    }
    for n in 1..1000 {
        let name = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let p = downloads.join(name);
        if !p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    Err(AppError::Internal(
        "too many conflicting downloads in ~/Downloads".into(),
    ))
}

fn split_stem_ext(name: &str) -> (String, String) {
    // Dotfile (".env", ".bashrc") — keep as-is, no extension split.
    if name.starts_with('.') && !name[1..].contains('.') {
        return (name.to_string(), String::new());
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), ext.to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// Open the multi-select file picker and return the picked paths along with
/// pre-allocated transfer ids the renderer can use to subscribe to progress
/// events BEFORE kicking off each upload. The renderer then issues N
/// sftp_upload_local invocations (one per file) in parallel, each tagged
/// with its transfer_id; each runs to completion in the background and the
/// terminal stays responsive throughout.
#[derive(Serialize)]
pub struct PickedUpload {
    pub local_path: String,
    pub transfer_id: String,
    pub is_dir: bool,
}

#[tauri::command]
pub async fn pick_uploads(app: AppHandle) -> AppResult<Vec<PickedUpload>> {
    let paths = pick_files(app).await?;
    Ok(paths
        .into_iter()
        .map(|p| PickedUpload {
            local_path: p,
            transfer_id: uuid::Uuid::new_v4().to_string(),
            is_dir: false,
        })
        .collect())
}

#[tauri::command]
pub async fn pick_upload_folder() -> AppResult<Option<PickedUpload>> {
    let Some(local_path) = pick_folder().await? else {
        return Ok(None);
    };
    Ok(Some(PickedUpload {
        local_path,
        transfer_id: uuid::Uuid::new_v4().to_string(),
        is_dir: true,
    }))
}

/// One native picker that lets the user choose any mix of files AND folders in
/// a single pass — so the Upload button is a single affordance instead of two.
/// `is_dir` is resolved per path from the real filesystem metadata, so each
/// selection routes to the file vs. folder SFTP path correctly downstream.
///
/// macOS: NSOpenPanel via JXA with canChooseFiles + canChooseDirectories +
/// multiple-selection (the AppleScript Standard Additions `choose file` /
/// `choose folder` verbs can't do both at once, which is why we drive the
/// panel directly). Other platforms fall back to the multi-file picker.
#[tauri::command]
pub async fn pick_uploads_any(app: AppHandle) -> AppResult<Vec<PickedUpload>> {
    #[cfg(target_os = "macos")]
    let paths: Vec<String> = {
        // NSOpenPanel must run on the main thread, and it only displays when
        // presented in-process (an osascript-spawned panel returns "cancel"
        // immediately without ever showing). Hop to the main thread, present
        // a modal panel that allows both files and directories, and ship the
        // chosen POSIX paths back over a channel.
        let (tx, rx) = tokio::sync::oneshot::channel::<Vec<String>>();
        app.run_on_main_thread(move || {
            let _ = tx.send(macos_pick_files_or_folders());
        })
        .map_err(|e| {
            AppError::Internal(format!(
                "failed to dispatch file picker to main thread: {e}"
            ))
        })?;
        rx.await
            .map_err(|_| AppError::Internal("file picker task was dropped".into()))?
    };
    #[cfg(not(target_os = "macos"))]
    let paths: Vec<String> = pick_files(app).await?;

    Ok(paths
        .into_iter()
        .map(|local_path| {
            let is_dir = std::fs::metadata(&local_path)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            PickedUpload {
                local_path,
                transfer_id: uuid::Uuid::new_v4().to_string(),
                is_dir,
            }
        })
        .collect())
}

/// Present a native NSOpenPanel (files + folders + multi-select) and return the
/// chosen absolute paths. MUST be called on the main thread. Returns an empty
/// vec on cancel. NSModalResponseOK == 1.
#[cfg(target_os = "macos")]
fn macos_pick_files_or_folders() -> Vec<String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSOpenPanel};

    let Some(mtm) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(true);
    panel.setResolvesAliases(true);
    // Make sure the panel comes to the front even if focus shifted.
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);

    let response = panel.runModal();
    if response != 1 {
        return Vec::new();
    }
    let urls = panel.URLs();
    let mut out = Vec::with_capacity(urls.count());
    for i in 0..urls.count() {
        let url = urls.objectAtIndex(i);
        if let Some(path) = url.path() {
            out.push(path.to_string());
        }
    }
    out
}

/// Reveal a local file in the OS file manager (Finder on macOS, Explorer on
/// Windows, xdg-open of the parent dir on Linux). Used by the SFTP browser
/// after a download completes so the user can click "Reveal" instead of
/// hunting through ~/Downloads.
#[tauri::command]
pub async fn reveal_in_finder(path: String) -> AppResult<()> {
    if path.contains('\0') {
        return Err(AppError::Invalid("path contains a NUL byte".into()));
    }
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err(AppError::Invalid(format!("path does not exist: {path}")));
    }
    #[cfg(target_os = "macos")]
    {
        tokio::process::Command::new("open")
            .arg("-R")
            .arg(&p)
            .status()
            .await
            .map_err(|e| AppError::Internal(format!("open -R failed: {e}")))?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        tokio::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", p.display()))
            .status()
            .await
            .map_err(|e| AppError::Internal(format!("explorer failed: {e}")))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let parent = p.parent().unwrap_or(std::path::Path::new("/"));
        tokio::process::Command::new("xdg-open")
            .arg(parent)
            .status()
            .await
            .map_err(|e| AppError::Internal(format!("xdg-open failed: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        applescript_string_literal, copy_local_image_to_clipboard_impl,
        extract_project_digest_from_response, normalize_host_input, parse_env_json,
        parse_prompt_enhancer_output, summarize_prompt_tool_call, validate_env_json,
        validate_export_file_path, validate_host_auth, validate_known_host_fingerprint,
        validate_local_output_file_path, validate_private_key_import_path, validate_pty_size,
        validate_remote_chmod_mode, validate_remote_file_output_path,
        validate_remote_operation_path, validate_snippet_input, validate_startup_snippet,
        validate_terminal_input_len, validate_tunnel_input, MAX_CLIPBOARD_IMAGE_BYTES,
    };
    use crate::vault::{AddHostInput, AddSnippetInput, AddTunnelInput};

    fn host_input(auth_kind: &str, key_path: Option<&str>) -> AddHostInput {
        AddHostInput {
            label: "  ".into(),
            hostname: "  192.0.2.10  ".into(),
            port: 22,
            username: "  root  ".into(),
            auth_kind: auth_kind.into(),
            key_path: key_path.map(str::to_string),
            group_name: None,
            os: None,
            jump_host_id: None,
            env_json: None,
            startup_snippet: None,
        }
    }

    fn snippet_input(label: &str, command: &str) -> AddSnippetInput {
        AddSnippetInput {
            label: label.into(),
            command: command.into(),
            description: None,
            tags: None,
            group_path: None,
        }
    }

    fn tunnel_input(kind: &str, local_port: i64, remote_port: Option<i64>) -> AddTunnelInput {
        AddTunnelInput {
            host_id: "host-1".into(),
            label: "Tunnel".into(),
            kind: kind.into(),
            local_port,
            remote_host: Some("127.0.0.1".into()),
            remote_port,
        }
    }

    #[test]
    fn host_label_is_optional_and_defaults_to_hostname() {
        let input = normalize_host_input(host_input("password", None));
        assert_eq!(input.hostname, "192.0.2.10");
        assert_eq!(input.username, "root");
        assert_eq!(input.label, "192.0.2.10");
    }

    #[test]
    fn key_auth_requires_key_path_but_password_does_not() {
        assert!(validate_host_auth(&host_input("password", None)).is_ok());
        assert!(validate_host_auth(&host_input("key_file", None)).is_err());
        assert!(
            validate_host_auth(&host_input("key_file", Some("/Users/me/.ssh/id_ed25519"))).is_ok()
        );
    }

    #[test]
    fn env_json_parser_only_returns_string_pairs() {
        assert_eq!(
            parse_env_json(Some(r#"{"TERM":"xterm-256color","LANG":"en_US.UTF-8"}"#)),
            vec![
                ("LANG".to_string(), "en_US.UTF-8".to_string()),
                ("TERM".to_string(), "xterm-256color".to_string()),
            ],
        );
        assert!(parse_env_json(Some(r#"{"BAD":1}"#)).is_empty());
        assert!(parse_env_json(None).is_empty());
    }

    #[test]
    fn env_json_validation_rejects_bad_or_excessive_values() {
        assert!(validate_env_json(Some(r#"{"TERM":"xterm-256color"}"#)).is_ok());
        assert!(validate_env_json(Some(r#"{"1BAD":"x"}"#)).is_err());
        assert!(validate_env_json(Some(r#"{"BAD-NAME":"x"}"#)).is_err());
        assert!(validate_env_json(Some(r#"{"BAD":1}"#)).is_err());

        let too_many = format!(
            "{{{}}}",
            (0..129)
                .map(|i| format!(r#""KEY{i}":"x""#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(validate_env_json(Some(&too_many)).is_err());

        let too_large_value = format!(r#"{{"KEY":"{}"}}"#, "x".repeat(4097));
        assert!(validate_env_json(Some(&too_large_value)).is_err());
    }

    #[test]
    fn startup_snippet_validation_rejects_huge_payloads() {
        assert!(validate_startup_snippet(Some("echo ready")).is_ok());
        assert!(validate_startup_snippet(Some(&"x".repeat(128 * 1024))).is_ok());
        assert!(validate_startup_snippet(Some(&"x".repeat(128 * 1024 + 1))).is_err());
    }

    #[test]
    fn applescript_string_literal_escapes_dialog_default_names() {
        assert_eq!(
            applescript_string_literal("bad \"name\"\\with\nline"),
            "bad \\\"name\\\"\\\\with line"
        );
    }

    #[test]
    fn vault_export_paths_reject_dangerous_targets() {
        assert!(validate_export_file_path("").is_err());
        assert!(validate_export_file_path("vault.json").is_err());
        assert!(validate_export_file_path("/").is_err());
        assert!(validate_export_file_path("/tmp/tersh-vault-export.json").is_ok());
        if let Ok(home) = std::env::var("HOME") {
            assert!(validate_export_file_path(&home).is_err());
        }
    }

    #[test]
    fn local_output_paths_reject_dangerous_targets() {
        assert!(validate_local_output_file_path("").is_err());
        assert!(validate_local_output_file_path("download.bin").is_err());
        assert!(validate_local_output_file_path("/").is_err());
        assert!(validate_local_output_file_path("/tmp/../tersh-download.bin").is_err());
        assert!(validate_local_output_file_path("/tmp/tersh-download.bin").is_ok());
        if let Ok(home) = std::env::var("HOME") {
            assert!(validate_local_output_file_path(&home).is_err());
        }
    }

    #[test]
    fn terminal_pty_sizes_reject_invalid_dimensions() {
        assert!(validate_pty_size(80, 24).is_ok());
        assert!(validate_pty_size(9, 24).is_err());
        assert!(validate_pty_size(80, 2).is_err());
        assert!(validate_pty_size(501, 24).is_err());
        assert!(validate_pty_size(80, 201).is_err());
    }

    #[test]
    fn terminal_input_rejects_oversized_payloads() {
        assert!(validate_terminal_input_len(0).is_ok());
        assert!(validate_terminal_input_len(4 * 1024 * 1024).is_ok());
        assert!(validate_terminal_input_len(4 * 1024 * 1024 + 1).is_err());
    }

    #[tokio::test]
    async fn clipboard_image_rejects_oversized_files_before_invoking_osascript() {
        let path = std::env::temp_dir().join(format!(
            "tersh-oversized-clipboard-{}.png",
            uuid::Uuid::new_v4()
        ));
        let file = std::fs::File::create(&path).expect("create temp image");
        file.set_len(MAX_CLIPBOARD_IMAGE_BYTES + 1)
            .expect("size temp image");

        let result = copy_local_image_to_clipboard_impl(path.to_str().unwrap()).await;
        let _ = std::fs::remove_file(&path);

        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("image is too large"));
    }

    #[test]
    fn snippet_validation_rejects_empty_or_oversized_values() {
        assert!(validate_snippet_input(&snippet_input("Deploy", "ls -la")).is_ok());
        assert!(validate_snippet_input(&snippet_input("", "ls -la")).is_err());
        assert!(validate_snippet_input(&snippet_input("Deploy", "   ")).is_err());
        assert!(validate_snippet_input(&snippet_input(&"x".repeat(121), "ls -la")).is_err());
        assert!(
            validate_snippet_input(&snippet_input("Deploy", &"x".repeat(128 * 1024 + 1))).is_err()
        );

        let mut input = snippet_input("Deploy", "ls -la");
        input.tags = Some("x".repeat(513));
        assert!(validate_snippet_input(&input).is_err());
    }

    #[test]
    fn tunnel_validation_rejects_invalid_ports_and_missing_hosts() {
        assert!(validate_tunnel_input(&tunnel_input("dynamic", 1080, None)).is_ok());
        assert!(validate_tunnel_input(&tunnel_input("local", 8080, Some(80))).is_ok());
        assert!(validate_tunnel_input(&tunnel_input("remote", 2222, Some(22))).is_ok());
        assert!(validate_tunnel_input(&tunnel_input("dynamic", 0, None)).is_err());
        assert!(validate_tunnel_input(&tunnel_input("dynamic", 65_536, None)).is_err());
        assert!(validate_tunnel_input(&tunnel_input("local", 8080, Some(0))).is_err());
        assert!(validate_tunnel_input(&tunnel_input("local", 8080, Some(65_536))).is_err());
        assert!(validate_tunnel_input(&tunnel_input("weird", 8080, Some(80))).is_err());

        let mut missing_remote = tunnel_input("local", 8080, Some(80));
        missing_remote.remote_host = Some("   ".into());
        assert!(validate_tunnel_input(&missing_remote).is_err());
    }

    #[test]
    fn key_import_path_requires_private_absolute_file() {
        assert!(validate_private_key_import_path(std::path::Path::new("id_ed25519")).is_err());
        assert!(
            validate_private_key_import_path(std::path::Path::new("/tmp/id_ed25519.pub")).is_err()
        );
        assert!(validate_private_key_import_path(std::path::Path::new("/")).is_err());
        assert!(
            validate_private_key_import_path(std::path::Path::new("/tmp/../id_ed25519")).is_err()
        );

        let path =
            std::env::temp_dir().join(format!("tersh-test-key-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&path, b"not a real key but enough for path validation").unwrap();
        assert!(validate_private_key_import_path(&path).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_mutation_paths_reject_root_relative_and_empty_values() {
        assert!(validate_remote_operation_path("/root/file.txt").is_ok());
        assert!(validate_remote_operation_path("").is_err());
        assert!(validate_remote_operation_path("relative/file.txt").is_err());
        assert!(validate_remote_operation_path("~").is_err());
        assert!(validate_remote_operation_path("/").is_err());
        assert!(validate_remote_operation_path("/tmp/bad\0name").is_err());
    }

    #[test]
    fn remote_file_output_paths_require_file_names() {
        assert!(validate_remote_file_output_path("/root/file.txt").is_ok());
        assert!(validate_remote_file_output_path("/root/").is_err());
        assert!(validate_remote_file_output_path("/").is_err());
        assert!(validate_remote_file_output_path("file.txt").is_err());
    }

    #[test]
    fn chmod_modes_reject_bits_outside_unix_permission_range() {
        assert!(validate_remote_chmod_mode(0o644).is_ok());
        assert!(validate_remote_chmod_mode(0o7777).is_ok());
        assert!(validate_remote_chmod_mode(0o10000).is_err());
    }

    #[test]
    fn known_host_fingerprints_require_sha256_shape() {
        assert!(validate_known_host_fingerprint("SHA256:abcdEFGH0123+/abcdEFGH0123").is_ok());
        assert!(validate_known_host_fingerprint("SHA256:abcdEFGH0123-_abcdEFGH0123").is_ok());
        assert!(validate_known_host_fingerprint("MD5:aa:bb:cc").is_err());
        assert!(validate_known_host_fingerprint(" SHA256:abcdEFGH0123+/abcdEFGH0123").is_err());
        assert!(validate_known_host_fingerprint("SHA256:short").is_err());
        assert!(validate_known_host_fingerprint("SHA256:abcdEFGH0123+/bad=").is_err());
    }

    #[test]
    fn prompt_enhancer_output_parser_splits_interpretation_from_prompt() {
        let parsed = parse_prompt_enhancer_output(
            "Interpretation:\nYou want a focused bug fix without changing unrelated UI.\n\nEnhanced prompt:\nFix the SSH input latency bug. Inspect the terminal input path first.\n\nQuestions:\nNone",
        );

        assert_eq!(
            parsed.interpretation.as_deref(),
            Some("You want a focused bug fix without changing unrelated UI.")
        );
        assert_eq!(
            parsed.enhanced_prompt,
            "Fix the SSH input latency bug. Inspect the terminal input path first."
        );
    }

    #[test]
    fn prompt_enhancer_output_parser_falls_back_to_raw_text() {
        let parsed = parse_prompt_enhancer_output("Fix the upload crash cleanly.");

        assert_eq!(parsed.interpretation, None);
        assert_eq!(parsed.enhanced_prompt, "Fix the upload crash cleanly.");
    }

    #[test]
    fn prompt_enhancer_output_parser_drops_stray_questions() {
        // The enhancer never asks. If a model disobeys and emits a Questions
        // section anyway, it must be cut OFF the rewrite, not leaked into it.
        let parsed = parse_prompt_enhancer_output(
            "Interpretation:\nYou want a performance baseline change.\n\nEnhanced prompt:\nReplace the raw HTTP-over-SSH benchmark with an OpenSSH -L baseline.\n\nQuestions:\n1. Keep the old benchmark for comparison?\n2. Use the same test server credentials?",
        );

        assert_eq!(
            parsed.enhanced_prompt,
            "Replace the raw HTTP-over-SSH benchmark with an OpenSSH -L baseline."
        );
    }

    #[test]
    fn project_digest_parser_treats_empty_provider_text_as_absent() {
        let parsed = extract_project_digest_from_response(
            r#"{"choices":[{"message":{"content":"   \n\t  "}}]}"#,
        )
        .expect("parse provider response");

        assert_eq!(parsed, None);
    }

    #[test]
    fn prompt_tool_trace_summarizes_safe_targets() {
        let grep = summarize_prompt_tool_call(
            "grep",
            &serde_json::json!({ "pattern": "TerminalView", "path": "frontend/src" }),
        );
        let find = summarize_prompt_tool_call(
            "find_files",
            &serde_json::json!({ "query": "Drawer", "path": "." }),
        );

        assert_eq!(grep.as_deref(), Some("TerminalView in frontend/src"));
        assert_eq!(find.as_deref(), Some("Drawer"));
    }
}
