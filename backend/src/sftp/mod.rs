use crate::errors::{AppError, AppResult};
use crate::ssh::SshSession;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use std::path::{Path, PathBuf};
use std::io::SeekFrom;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 256 KB chunk — about 4× the SFTP packet size, big enough to keep the wire
/// saturated without buffering more than necessary in memory.
const TRANSFER_BUF: usize = 256 * 1024;

/// Keep the transfer HUD responsive without flooding the frontend.
const PROGRESS_EVERY_BYTES: u64 = 1024 * 1024;

/// Single-RPC operations (list, rename, mkdir, chmod, remove) get a hard
/// timeout so a wedged server can't hang the Tauri command forever. Transfers
/// (upload/download) intentionally don't use this — they're long-running by
/// design and should be cancelled explicitly via the transfer queue.
const SFTP_OP_TIMEOUT: Duration = Duration::from_secs(30);

async fn with_timeout<F, T>(label: &str, fut: F) -> AppResult<T>
where
    F: std::future::Future<Output = AppResult<T>>,
{
    tokio::time::timeout(SFTP_OP_TIMEOUT, fut)
        .await
        .map_err(|_| AppError::Sftp(format!("{label} timed out after {SFTP_OP_TIMEOUT:?}")))?
}

#[derive(serde::Serialize, Clone)]
pub struct TransferProgress {
    pub transfer_id: String,
    pub path: String,
    pub bytes_done: u64,
    pub total: u64,
    pub done: bool,
}

#[derive(serde::Serialize)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: i64,
}

/// Hard cap on entries returned by list_dir / list_local_dir. Directories
/// with 50k+ entries (think `/usr/share`, `/nix/store`, node_modules) would
/// otherwise pin the renderer for seconds during JSON parse + DOM build, on
/// top of holding the SFTP channel during the readdir itself. Frontend
/// shows the truncated flag so the user knows.
const MAX_DIR_ENTRIES: usize = 5_000;

/// Result of a directory listing — entries plus a "more were available but
/// we capped" flag so the UI can tell the user there's hidden content.
pub struct DirListing {
    pub cwd: String,
    pub entries: Vec<RemoteEntry>,
    pub truncated: bool,
}

/// List a remote directory via SFTP.
///
/// On first failure, drop the cached SFTP session and reopen. invalidate_sftp
/// now waits for any in-flight ops (upload/download/rename) to finish before
/// tearing the subsystem down, so the retry can't yank the rug out from under
/// concurrent operations.
pub async fn list_dir(session: &SshSession, path: &str) -> AppResult<DirListing> {
    let guard = session.acquire_sftp().await?;
    let first = with_timeout("sftp list_dir", list_dir_with_sftp(guard.sftp(), path)).await;
    match first {
        Ok(listing) => Ok(listing),
        Err(first_err) => {
            // Release our refcount before invalidating, so invalidate_sftp's
            // drain wait can succeed.
            drop(guard);
            session.invalidate_sftp().await;
            let guard = session.acquire_sftp().await?;
            with_timeout(
                "sftp list_dir (retry)",
                list_dir_with_sftp(guard.sftp(), path),
            )
            .await
            .map_err(|second_err| {
                AppError::Sftp(format!(
                    "{first_err}; retry after reopening sftp failed: {second_err}"
                ))
            })
        }
    }
}

async fn list_dir_with_sftp(sftp: &SftpSession, path: &str) -> AppResult<DirListing> {
    let abs = if path.is_empty() || path == "~" {
        sftp.canonicalize(".")
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize: {e}")))?
    } else if let Some(rest) = path.strip_prefix("~/") {
        let home = sftp
            .canonicalize(".")
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize home: {e}")))?;
        let expanded = join_home_path(&home, rest);
        sftp.canonicalize(&expanded)
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize {path}: {e}")))?
    } else {
        sftp.canonicalize(path)
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize {path}: {e}")))?
    };

    let mut entries: Vec<RemoteEntry> = Vec::new();
    let mut truncated = false;
    let dir = sftp
        .read_dir(&abs)
        .await
        .map_err(|e| AppError::Sftp(format!("read_dir {abs}: {e}")))?;
    for entry in dir {
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        if entries.len() >= MAX_DIR_ENTRIES {
            truncated = true;
            break;
        }
        let meta = entry.metadata();
        let full = if abs.ends_with('/') {
            format!("{abs}{name}")
        } else {
            format!("{abs}/{name}")
        };
        entries.push(RemoteEntry {
            name,
            path: full,
            is_dir: meta.is_dir(),
            size: meta.size.unwrap_or(0),
            modified: meta.mtime.map(|m| m as i64).unwrap_or(0),
        });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(DirListing {
        cwd: abs,
        entries,
        truncated,
    })
}

fn join_home_path(home: &str, rest: &str) -> String {
    let suffix = rest.trim_start_matches('/');
    if suffix.is_empty() {
        home.to_string()
    } else if home == "/" {
        format!("/{suffix}")
    } else {
        format!("{}/{}", home.trim_end_matches('/'), suffix)
    }
}

async fn canonical_remote_dir(sftp: &SftpSession, dir: &str, label: &str) -> AppResult<String> {
    let expanded = expand_remote_dir_reference(sftp, dir, label).await?;
    sftp.canonicalize(&expanded)
        .await
        .map_err(|e| AppError::Sftp(format!("canonicalize {label} {expanded}: {e}")))
}

async fn expand_remote_dir_reference(
    sftp: &SftpSession,
    dir: &str,
    label: &str,
) -> AppResult<String> {
    if dir == "~" {
        return sftp
            .canonicalize(".")
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize {label} home: {e}")));
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        let home = sftp
            .canonicalize(".")
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize {label} home: {e}")))?;
        return Ok(join_home_path(&home, rest));
    }
    Ok(dir.to_string())
}

fn join_remote_child(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

fn split_remote_name(name: &str) -> (&str, &str) {
    if let Some(rest) = name.strip_prefix('.') {
        if !rest.contains('.') {
            return (name, "");
        }
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

async fn remote_file_exists(sftp: &SftpSession, path: &str) -> AppResult<bool> {
    match sftp.metadata(path).await {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

async fn unique_remote_child_path(
    sftp: &SftpSession,
    parent: &str,
    basename: &str,
) -> AppResult<(String, String)> {
    let first = join_remote_child(parent, basename);
    if !with_timeout(
        "sftp upload check destination",
        remote_file_exists(sftp, &first),
    )
    .await?
    {
        return Ok((first, basename.to_string()));
    }

    let (stem, ext) = split_remote_name(basename);
    for n in 1..=999 {
        let candidate_name = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let candidate_path = join_remote_child(parent, &candidate_name);
        if !with_timeout(
            "sftp upload check destination",
            remote_file_exists(sftp, &candidate_path),
        )
        .await?
        {
            return Ok((candidate_path, candidate_name));
        }
    }

    Err(AppError::Sftp(format!(
        "upload destination already has too many copies of {basename}"
    )))
}

/// Upload a local file to a remote directory chosen by the caller.
/// Returns the remote absolute path that was written.
///
/// `progress` (AppHandle + transfer_id) gets `sftp://transfer/<id>/progress`
/// events so the renderer can show a live progress bar. `cancel` lets the UI
/// abort mid-chunk — the temp file is removed on cancel/error so the remote
/// never sees a half-written file at the final path. Without these the
/// terminal would freeze for the entire upload because the Tauri command
/// would block silently with no feedback.
///
/// SECURITY: caller canonicalizes and validates the local path immediately
/// before passing it here.
#[allow(dead_code)]
pub async fn upload_to_agent_dir(
    sftp: &SftpSession,
    local_path: &str,
    upload_dir: &str,
    upload_dir_is_agent_cwd: bool,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
) -> AppResult<UploadResult> {
    let prepared = prepare_upload_to_agent_dir(sftp, local_path, upload_dir).await?;
    upload_prepared_file(
        sftp,
        local_path,
        &prepared,
        upload_dir_is_agent_cwd,
        progress,
        cancel,
        true,
    )
    .await
}

#[derive(Clone)]
pub struct PreparedUpload {
    pub remote_path: String,
    remote_name: String,
    temp_path: String,
    total_size: u64,
}

pub async fn prepare_upload_to_agent_dir(
    sftp: &SftpSession,
    local_path: &str,
    upload_dir: &str,
) -> AppResult<PreparedUpload> {
    // 1. Sanity-check local file.
    let meta = tokio::fs::metadata(local_path)
        .await
        .map_err(|e| AppError::Sftp(format!("stat local upload source: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Invalid("local path is not a file".into()));
    }
    let total_size = meta.len();

    // 2. Compute remote path from the caller-provided upload dir. Prefer the
    // detected agent cwd when available; otherwise callers can pass a neutral
    // inbox such as /tmp/tersh-agent-inbox/<session>. The upload still streams
    // over a separate SFTP transport so large uploads do not freeze the
    // interactive terminal.
    let basename = Path::new(local_path)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| AppError::Invalid("bad local filename".into()))?;
    let safe_basename = sanitize_basename(basename);
    let session_dir = upload_dir.trim_end_matches('/').to_string();

    // 3. Ensure directory exists (mkdir -p semantics, manual since SFTP has no -p).
    // Single-RPC ops here all get the standard SFTP_OP_TIMEOUT so a wedged
    // server can't hang the drag-drop indefinitely.
    with_timeout("sftp upload_session mkdir_p", mkdir_p(sftp, &session_dir)).await?;
    let canonical_session_dir = with_timeout(
        "sftp upload_session canonical_dir",
        canonical_remote_dir(sftp, &session_dir, "upload directory"),
    )
    .await?;
    let (remote_path, remote_name) =
        unique_remote_child_path(sftp, &canonical_session_dir, &safe_basename).await?;

    let temp_path = format!(
        "{remote_path}.{}.tersh-upload",
        uuid::Uuid::new_v4().simple()
    );
    Ok(PreparedUpload {
        remote_path,
        remote_name,
        temp_path,
        total_size,
    })
}

/// Continue a prepared single-file upload. If a previous attempt left the
/// staging file behind, the next attempt resumes from its current remote size.
/// The final path is only published after the whole file lands cleanly.
pub async fn upload_prepared_file(
    sftp: &SftpSession,
    local_path: &str,
    prepared: &PreparedUpload,
    upload_dir_is_agent_cwd: bool,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
    cleanup_on_error: bool,
) -> AppResult<UploadResult> {
    let remote_path = prepared.remote_path.clone();
    let temp_path = prepared.temp_path.clone();
    let total_size = prepared.total_size;

    let mut resume_from = match sftp.metadata(&temp_path).await {
        Ok(attrs) => attrs.size.unwrap_or(0).min(total_size),
        Err(_) => 0,
    };
    if resume_from > total_size {
        let _ = tokio::time::timeout(SFTP_OP_TIMEOUT, sftp.remove_file(&temp_path)).await;
        resume_from = 0;
    }

    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| AppError::Sftp(format!("open local: {e}")))?;
    if resume_from > 0 {
        local_file
            .seek(SeekFrom::Start(resume_from))
            .await
            .map_err(|e| AppError::Sftp(format!("seek local upload source: {e}")))?;
    }

    let open_flags = if resume_from == 0 {
        OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE
    } else {
        OpenFlags::CREATE | OpenFlags::WRITE
    };
    let open_fut = async {
        sftp.open_with_flags(
            &temp_path,
            open_flags,
        )
        .await
        .map_err(|e| AppError::Sftp(format!("open remote {temp_path}: {e}")))
    };
    let mut remote_file = match with_timeout("sftp upload_session open_remote", open_fut).await {
        Ok(f) => f,
        Err(e) => return Err(e),
    };
    if resume_from > 0 {
        remote_file
            .seek(SeekFrom::Start(resume_from))
            .await
            .map_err(|e| AppError::Sftp(format!("seek remote {temp_path}: {e}")))?;
    }

    let mut buf = vec![0u8; TRANSFER_BUF];
    let mut total = resume_from;
    let mut next_emit = total.saturating_add(PROGRESS_EVERY_BYTES);
    if total > 0 {
        emit_progress(progress.as_ref(), &remote_path, total, total_size, false);
    }
    let result: AppResult<()> = loop {
        if let Some(ref c) = cancel {
            if c.load(Ordering::Acquire) {
                break Err(AppError::Sftp("upload cancelled".into()));
            }
        }
        let n = match local_file.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => break Err(AppError::Sftp(format!("read local: {e}"))),
        };
        if n == 0 {
            break Ok(());
        }
        if let Err(e) = remote_file.write_all(&buf[..n]).await {
            break Err(AppError::Sftp(format!("write remote: {e}")));
        }
        total += n as u64;
        if total >= next_emit {
            emit_progress(progress.as_ref(), &remote_path, total, total_size, false);
            next_emit = total + PROGRESS_EVERY_BYTES;
        }
    };
    let _ = remote_file.shutdown().await;

    // If we placed the file directly in the agent's CWD, the bare filename is
    // the most natural reference in the prompt. For neutral inbox uploads we
    // must return the absolute path so the agent can find it from any folder.
    let agent_relative_name = upload_dir_is_agent_cwd.then_some(prepared.remote_name.clone());

    match result {
        Ok(()) => {
            let rename_fut = async {
                sftp.rename(&temp_path, &remote_path).await.map_err(|e| {
                    AppError::Sftp(format!("rename {temp_path} -> {remote_path}: {e}"))
                })
            };
            if let Err(e) = with_timeout("sftp upload_session rename", rename_fut).await {
                let _ = tokio::time::timeout(SFTP_OP_TIMEOUT, sftp.remove_file(&temp_path)).await;
                emit_progress(
                    progress.as_ref(),
                    &remote_path,
                    total,
                    total_size.max(total),
                    true,
                );
                return Err(e);
            }
            emit_progress(
                progress.as_ref(),
                &remote_path,
                total,
                total_size.max(total),
                true,
            );
            Ok(UploadResult {
                remote_path,
                bytes_written: total,
                agent_relative_name,
            })
        }
        Err(e) => {
            if cleanup_on_error {
                let _ = tokio::time::timeout(SFTP_OP_TIMEOUT, sftp.remove_file(&temp_path)).await;
            }
            emit_progress(
                progress.as_ref(),
                &remote_path,
                total,
                total_size.max(total),
                true,
            );
            Err(e)
        }
    }
}

#[derive(serde::Serialize)]
pub struct UploadResult {
    pub remote_path: String,
    pub bytes_written: u64,
    /// When the upload landed in the AI agent's current working directory,
    /// this is the bare basename for use in the user's prompt (so they can
    /// just `@filename` instead of `@/long/full/path`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_relative_name: Option<String>,
}

#[derive(serde::Serialize)]
pub struct UploadFolderResult {
    pub remote_path: String,
    pub bytes_written: u64,
    pub files_uploaded: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_relative_name: Option<String>,
}

struct FolderUploadEntry {
    local_path: String,
    relative_parts: Vec<String>,
    size: u64,
}

struct FolderUploadPlan {
    folder_name: String,
    files: Vec<FolderUploadEntry>,
    total_size: u64,
}

pub async fn upload_folder_to_agent_dir(
    sftp: &SftpSession,
    local_dir: &str,
    upload_dir: &str,
    upload_dir_is_agent_cwd: bool,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
) -> AppResult<UploadFolderResult> {
    let plan = collect_folder_upload_plan(local_dir).await?;
    let session_dir = upload_dir.trim_end_matches('/').to_string();
    with_timeout("sftp upload_folder mkdir_p", mkdir_p(sftp, &session_dir)).await?;
    let canonical_session_dir = with_timeout(
        "sftp upload_folder canonical_dir",
        canonical_remote_dir(sftp, &session_dir, "upload directory"),
    )
    .await?;
    let safe_folder_name = sanitize_basename(&plan.folder_name);
    let (remote_root, remote_name) =
        unique_remote_child_path(sftp, &canonical_session_dir, &safe_folder_name).await?;
    let (bytes_written, files_uploaded) =
        upload_folder_plan_to_root(sftp, plan, &remote_root, progress, cancel).await?;

    Ok(UploadFolderResult {
        remote_path: remote_root,
        bytes_written,
        files_uploaded,
        agent_relative_name: upload_dir_is_agent_cwd.then_some(remote_name),
    })
}

pub async fn upload_folder_to_dir_with_sftp(
    sftp: &SftpSession,
    local_dir: &str,
    remote_dir: &str,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
) -> AppResult<UploadFolderResult> {
    let plan = collect_folder_upload_plan(local_dir).await?;
    let remote_dir = expand_remote_dir_reference(sftp, remote_dir, "upload parent").await?;
    with_timeout("sftp upload_folder mkdir_p", mkdir_p(sftp, &remote_dir)).await?;
    let canonical_remote_dir = with_timeout(
        "sftp upload_folder canonical_parent",
        canonical_remote_dir(sftp, &remote_dir, "upload parent"),
    )
    .await?;
    let safe_folder_name = sanitize_basename(&plan.folder_name);
    let (remote_root, _) =
        unique_remote_child_path(sftp, &canonical_remote_dir, &safe_folder_name).await?;
    let (bytes_written, files_uploaded) =
        upload_folder_plan_to_root(sftp, plan, &remote_root, progress, cancel).await?;
    Ok(UploadFolderResult {
        remote_path: remote_root,
        bytes_written,
        files_uploaded,
        agent_relative_name: None,
    })
}

/// Upload to an arbitrary remote path (file-browser drop, not the per-session dir).
///
/// Uses an atomic staging pattern: data streams into `<path>.tersh-upload`
/// while the transfer is in flight, and the temp file is renamed to the final
/// path only on clean shutdown. Cancel / network failure / app crash leaves
/// a unique temp file rather than a truncated final file masquerading as
/// complete content.
///
/// `cancel` lets the UI abort the transfer mid-chunk. On cancel we shut down
/// the remote handle and delete the temp file before returning Err.
///
/// Emits sftp://transfer/<id>/progress events if a transfer_id and app handle
/// are provided. Set transfer_id=None for legacy/silent callers.
#[allow(dead_code)]
pub async fn upload_to_path_with_sftp(
    sftp: &SftpSession,
    local_path: &str,
    remote_path: &str,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
) -> AppResult<UploadResult> {
    let prepared = prepare_upload_to_path_with_sftp(sftp, local_path, remote_path).await?;
    upload_prepared_file(sftp, local_path, &prepared, false, progress, cancel, true).await
}

pub async fn prepare_upload_to_path_with_sftp(
    sftp: &SftpSession,
    local_path: &str,
    remote_path: &str,
) -> AppResult<PreparedUpload> {
    let meta = tokio::fs::metadata(local_path)
        .await
        .map_err(|e| AppError::Sftp(format!("stat local upload source: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Invalid("local path is not a file".into()));
    }
    let total_size = meta.len();
    let remote_name = Path::new(remote_path)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| AppError::Invalid("remote output path must include a file name".into()))?
        .to_string();
    let canonical_parent =
        if let Some(parent) = Path::new(remote_path).parent().and_then(|p| p.to_str()) {
            let dir = if parent.is_empty() { "/" } else { parent };
            let expanded_dir = expand_remote_dir_reference(sftp, dir, "upload parent").await?;
            if !expanded_dir.is_empty() && expanded_dir != "/" {
                with_timeout("sftp upload mkdir_p", mkdir_p(sftp, &expanded_dir)).await?;
            }
            with_timeout(
                "sftp upload canonical_parent",
                canonical_remote_dir(sftp, &expanded_dir, "upload parent"),
            )
            .await?
        } else {
            with_timeout(
                "sftp upload canonical_parent",
                canonical_remote_dir(sftp, ".", "upload parent"),
            )
            .await?
        };
    let remote_path = join_remote_child(&canonical_parent, &remote_name);
    let temp_path = format!(
        "{remote_path}.{}.tersh-upload",
        uuid::Uuid::new_v4().simple()
    );
    Ok(PreparedUpload {
        remote_path,
        remote_name,
        temp_path,
        total_size,
    })
}

pub async fn upload_to_path_prepared_with_sftp(
    sftp: &SftpSession,
    local_path: &str,
    prepared: &PreparedUpload,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
    cleanup_on_error: bool,
) -> AppResult<UploadResult> {
    upload_prepared_file(sftp, local_path, prepared, false, progress, cancel, cleanup_on_error).await
}

async fn collect_folder_upload_plan(local_dir: &str) -> AppResult<FolderUploadPlan> {
    let root = PathBuf::from(local_dir);
    tokio::task::spawn_blocking(move || collect_folder_upload_plan_blocking(root))
        .await
        .map_err(|e| AppError::Sftp(format!("scan upload folder task failed: {e}")))?
}

fn collect_folder_upload_plan_blocking(root: PathBuf) -> AppResult<FolderUploadPlan> {
    let root = root
        .canonicalize()
        .map_err(|e| AppError::Invalid(format!("upload folder unavailable: {e}")))?;
    if let Some(reason) = sensitive_path_reason(&root.to_string_lossy()) {
        return Err(AppError::Invalid(format!("sensitive_path:{reason}")));
    }
    let root_meta = std::fs::symlink_metadata(&root)
        .map_err(|e| AppError::Invalid(format!("stat upload folder: {e}")))?;
    if !root_meta.is_dir() {
        return Err(AppError::Invalid("local path is not a folder".into()));
    }
    let folder_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("folder")
        .to_string();

    let mut stack = vec![root.clone()];
    let mut files = Vec::new();
    let mut total_size = 0u64;
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| AppError::Sftp(format!("read local folder {}: {e}", dir.display())))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| AppError::Sftp(format!("read local folder entry: {e}")))?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| AppError::Sftp(format!("stat local {}: {e}", path.display())))?;
            if meta.file_type().is_symlink() {
                return Err(AppError::Invalid(format!(
                    "folder upload does not follow symlinks: {}",
                    path.display()
                )));
            }
            let path_str = path.to_string_lossy();
            if let Some(reason) = sensitive_path_reason(&path_str) {
                return Err(AppError::Invalid(format!("sensitive_path:{reason}")));
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            total_size = total_size
                .checked_add(meta.len())
                .ok_or_else(|| AppError::Invalid("folder upload is too large".into()))?;
            let relative = path.strip_prefix(&root).map_err(|e| {
                AppError::Sftp(format!(
                    "derive relative upload path {}: {e}",
                    path.display()
                ))
            })?;
            let mut parts = Vec::new();
            for part in relative.components() {
                let name = part.as_os_str().to_string_lossy().to_string();
                if name.is_empty() || name == "." || name == ".." {
                    return Err(AppError::Invalid("bad folder upload path".into()));
                }
                parts.push(name);
            }
            if parts.is_empty() {
                continue;
            }
            files.push(FolderUploadEntry {
                local_path: path.to_string_lossy().into_owned(),
                relative_parts: parts,
                size: meta.len(),
            });
            if files.len() > MAX_DIR_ENTRIES {
                return Err(AppError::Invalid(
                    "folder upload has too many files; upload a smaller folder or archive it"
                        .into(),
                ));
            }
        }
    }

    files.sort_by(|a, b| a.relative_parts.cmp(&b.relative_parts));
    Ok(FolderUploadPlan {
        folder_name,
        files,
        total_size,
    })
}

async fn upload_folder_plan_to_root(
    sftp: &SftpSession,
    plan: FolderUploadPlan,
    remote_root: &str,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
) -> AppResult<(u64, usize)> {
    with_timeout("sftp upload_folder mkdir_root", mkdir_p(sftp, remote_root)).await?;
    if plan.files.is_empty() {
        emit_progress(progress.as_ref(), remote_root, 0, 0, true);
        return Ok((0, 0));
    }

    let total_size = plan.total_size;
    let mut bytes_done = 0u64;
    let mut uploaded = 0usize;
    for entry in plan.files {
        if let Some(ref c) = cancel {
            if c.load(Ordering::Acquire) {
                emit_progress(progress.as_ref(), remote_root, bytes_done, total_size, true);
                return Err(AppError::Sftp("upload cancelled".into()));
            }
        }
        let mut remote_path = remote_root.to_string();
        for part in &entry.relative_parts {
            remote_path = join_remote_child(&remote_path, &sanitize_basename(part));
        }
        upload_file_to_exact_path(
            sftp,
            &entry.local_path,
            &remote_path,
            remote_root,
            total_size,
            &mut bytes_done,
            progress.as_ref(),
            cancel.as_ref(),
        )
        .await?;
        uploaded += 1;
        let _ = entry.size;
        emit_progress(
            progress.as_ref(),
            remote_root,
            bytes_done,
            total_size,
            false,
        );
    }
    emit_progress(progress.as_ref(), remote_root, bytes_done, total_size, true);
    Ok((bytes_done, uploaded))
}

async fn upload_file_to_exact_path(
    sftp: &SftpSession,
    local_path: &str,
    remote_path: &str,
    progress_path: &str,
    total_size: u64,
    bytes_done: &mut u64,
    progress: Option<&(AppHandle, String)>,
    cancel: Option<&Arc<AtomicBool>>,
) -> AppResult<()> {
    let parent = remote_path
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/");
    with_timeout(
        "sftp upload_folder mkdir_file_parent",
        mkdir_p(sftp, parent),
    )
    .await?;
    let temp_path = format!(
        "{remote_path}.{}.tersh-upload",
        uuid::Uuid::new_v4().simple()
    );
    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| AppError::Sftp(format!("open local: {e}")))?;
    let mut remote_file = with_timeout("sftp upload_folder open_remote", async {
        sftp.open_with_flags(
            &temp_path,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|e| AppError::Sftp(format!("open remote {temp_path}: {e}")))
    })
    .await?;
    let mut buf = vec![0u8; TRANSFER_BUF];
    let mut next_emit = bytes_done.saturating_add(PROGRESS_EVERY_BYTES);
    let result: AppResult<()> = loop {
        if let Some(c) = cancel {
            if c.load(Ordering::Acquire) {
                break Err(AppError::Sftp("upload cancelled".into()));
            }
        }
        let n = match local_file.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => break Err(AppError::Sftp(format!("read local: {e}"))),
        };
        if n == 0 {
            break Ok(());
        }
        if let Err(e) = remote_file.write_all(&buf[..n]).await {
            break Err(AppError::Sftp(format!("write remote: {e}")));
        }
        *bytes_done += n as u64;
        if *bytes_done >= next_emit {
            emit_progress(progress, progress_path, *bytes_done, total_size, false);
            next_emit = bytes_done.saturating_add(PROGRESS_EVERY_BYTES);
        }
    };
    let _ = remote_file.shutdown().await;
    match result {
        Ok(()) => {
            let rename_fut = async {
                sftp.rename(&temp_path, remote_path).await.map_err(|e| {
                    AppError::Sftp(format!("rename {temp_path} -> {remote_path}: {e}"))
                })
            };
            if let Err(e) = with_timeout("sftp upload_folder rename", rename_fut).await {
                let _ = tokio::time::timeout(SFTP_OP_TIMEOUT, sftp.remove_file(&temp_path)).await;
                return Err(e);
            }
            Ok(())
        }
        Err(e) => {
            let _ = tokio::time::timeout(SFTP_OP_TIMEOUT, sftp.remove_file(&temp_path)).await;
            Err(e)
        }
    }
}

/// Download a remote file to a local path. Caller is responsible for choosing
/// `local_path` (typically via native save dialog). `cancel` lets the UI abort
/// mid-chunk; on cancel the partially-written local file is removed.
pub async fn download_to_path(
    session: &SshSession,
    remote_path: &str,
    local_path: &str,
    progress: Option<(AppHandle, String)>,
    cancel: Option<Arc<AtomicBool>>,
) -> AppResult<u64> {
    let guard = session.acquire_sftp().await?;
    let sftp = guard.sftp();
    let remote_path = with_timeout("sftp download canonical_remote", async {
        sftp.canonicalize(remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("canonicalize remote {remote_path}: {e}")))
    })
    .await?;
    let total_size = with_timeout("sftp download metadata", async {
        sftp.metadata(&remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("metadata remote {remote_path}: {e}")))
    })
    .await
    .ok()
    .and_then(|m| m.size)
    .unwrap_or(0);
    let mut remote_file = with_timeout("sftp download open_remote", async {
        sftp.open(&remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("open remote {remote_path}: {e}")))
    })
    .await?;
    if let Some(parent) = Path::new(local_path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Sftp(format!("mkdir local {}: {e}", parent.display())))?;
        }
    }
    let mut local_file = tokio::fs::File::create(local_path)
        .await
        .map_err(|e| AppError::Sftp(format!("create local {local_path}: {e}")))?;
    let mut buf = vec![0u8; TRANSFER_BUF];
    let mut total = 0u64;
    let mut next_emit = PROGRESS_EVERY_BYTES;
    let result: AppResult<()> = loop {
        if let Some(ref c) = cancel {
            if c.load(Ordering::Acquire) {
                break Err(AppError::Sftp("download cancelled".into()));
            }
        }
        let n = match remote_file.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => break Err(AppError::Sftp(format!("read remote: {e}"))),
        };
        if n == 0 {
            break Ok(());
        }
        if let Err(e) = local_file.write_all(&buf[..n]).await {
            break Err(AppError::Sftp(format!("write local: {e}")));
        }
        total += n as u64;
        if total >= next_emit {
            emit_progress(progress.as_ref(), local_path, total, total_size, false);
            next_emit = total + PROGRESS_EVERY_BYTES;
        }
    };
    let _ = local_file.shutdown().await;
    match result {
        Ok(()) => {
            emit_progress(
                progress.as_ref(),
                local_path,
                total,
                total_size.max(total),
                true,
            );
            Ok(total)
        }
        Err(e) => {
            // Delete partial file so the user doesn't think the download
            // completed (the local file's bytes wouldn't match the remote).
            let _ = tokio::fs::remove_file(local_path).await;
            emit_progress(
                progress.as_ref(),
                local_path,
                total,
                total_size.max(total),
                true,
            );
            Err(e)
        }
    }
}

fn emit_progress(
    progress: Option<&(AppHandle, String)>,
    path: &str,
    bytes_done: u64,
    total: u64,
    done: bool,
) {
    let Some((app, transfer_id)) = progress else {
        return;
    };
    let payload = TransferProgress {
        transfer_id: transfer_id.clone(),
        path: path.to_string(),
        bytes_done,
        total,
        done,
    };
    let _ = app.emit(&format!("sftp://transfer/{transfer_id}/progress"), payload);
}

/// List a LOCAL directory. Mirrors RemoteEntry shape so the renderer can use
/// the same file-row component for both panes.
pub async fn list_local_dir(path: &str) -> AppResult<DirListing> {
    let path = if path.is_empty() || path == "~" {
        std::env::var("HOME").unwrap_or_else(|_| "/".into())
    } else {
        path.to_string()
    };
    let p = PathBuf::from(&path)
        .canonicalize()
        .map_err(|e| AppError::Sftp(format!("canonicalize local {path}: {e}")))?;
    let abs = p.to_string_lossy().into_owned();
    let mut entries: Vec<RemoteEntry> = Vec::new();
    let mut truncated = false;
    let mut rd = tokio::fs::read_dir(&p)
        .await
        .map_err(|e| AppError::Sftp(format!("read_dir local {abs}: {e}")))?;
    while let Some(entry) = rd.next_entry().await.transpose() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entries.len() >= MAX_DIR_ENTRIES {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let full = entry.path().to_string_lossy().into_owned();
        entries.push(RemoteEntry {
            name,
            path: full,
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified,
        });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(DirListing {
        cwd: abs,
        entries,
        truncated,
    })
}

pub async fn rename(session: &SshSession, from: &str, to: &str) -> AppResult<()> {
    let guard = session.acquire_sftp().await?;
    with_timeout("sftp rename", async move {
        guard
            .sftp()
            .rename(from, to)
            .await
            .map_err(|e| AppError::Sftp(format!("rename {from} -> {to}: {e}")))
    })
    .await
}

pub async fn mkdir(session: &SshSession, path: &str) -> AppResult<()> {
    let guard = session.acquire_sftp().await?;
    with_timeout("sftp mkdir", async move {
        guard
            .sftp()
            .create_dir(path)
            .await
            .map_err(|e| AppError::Sftp(format!("mkdir {path}: {e}")))
    })
    .await
}

pub async fn remove(session: &SshSession, path: &str, is_dir: bool) -> AppResult<()> {
    let guard = session.acquire_sftp().await?;
    if is_dir {
        remove_dir_recursive(guard.sftp(), path).await
    } else {
        with_timeout("sftp remove_file", async move {
            guard
                .sftp()
                .remove_file(path)
                .await
                .map_err(|e| AppError::Sftp(format!("rm {path}: {e}")))
        })
        .await
    }
}

async fn remove_dir_recursive(sftp: &SftpSession, root: &str) -> AppResult<()> {
    let root = sftp
        .canonicalize(root)
        .await
        .map_err(|e| AppError::Sftp(format!("canonicalize delete target {root}: {e}")))?;
    let mut stack = vec![(root, false)];

    while let Some((path, visited)) = stack.pop() {
        if visited {
            with_timeout("sftp remove_dir", async {
                sftp.remove_dir(&path)
                    .await
                    .map_err(|e| AppError::Sftp(format!("rmdir {path}: {e}")))
            })
            .await?;
            continue;
        }

        stack.push((path.clone(), true));
        let entries = with_timeout("sftp remove_dir read_dir", async {
            sftp.read_dir(&path)
                .await
                .map_err(|e| AppError::Sftp(format!("read_dir {path}: {e}")))
        })
        .await?;

        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child_path = join_remote_child(&path, &name);
            if entry.metadata().is_dir() {
                stack.push((child_path, false));
            } else {
                with_timeout("sftp remove_file", async {
                    sftp.remove_file(&child_path)
                        .await
                        .map_err(|e| AppError::Sftp(format!("rm {child_path}: {e}")))
                })
                .await?;
            }
        }
    }

    Ok(())
}

pub async fn chmod(session: &SshSession, path: &str, mode: u32) -> AppResult<()> {
    let guard = session.acquire_sftp().await?;
    with_timeout("sftp chmod", async move {
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };
        guard
            .sftp()
            .set_metadata(path, attrs)
            .await
            .map_err(|e| AppError::Sftp(format!("chmod {path}: {e}")))
    })
    .await
}

/// Largest remote file read_remote_bytes will pull into memory. The project
/// index store is JSON in the low single-digit MB; this guards against a
/// hostile/corrupt server handing back something enormous.
const MAX_REMOTE_READ_BYTES: u64 = 64 * 1024 * 1024;

/// Write an in-memory buffer to an absolute remote path atomically (temp file +
/// rename) with parent dirs created and 0600 perms. Used to persist the project
/// index store ON the VPS instead of the local Mac disk. `remote_path` must be
/// absolute (mkdir_p walks from `/`).
pub async fn write_remote_bytes(
    session: &SshSession,
    remote_path: &str,
    bytes: &[u8],
) -> AppResult<()> {
    let guard = session.acquire_sftp().await?;
    let sftp = guard.sftp();
    if let Some(parent) = Path::new(remote_path).parent().and_then(|p| p.to_str()) {
        if !parent.is_empty() && parent != "/" {
            with_timeout("sftp write_bytes mkdir_p", mkdir_p(sftp, parent)).await?;
        }
    }
    let temp_path = format!("{remote_path}.{}.tersh-tmp", uuid::Uuid::new_v4().simple());
    let open_fut = async {
        sftp.open_with_flags(
            &temp_path,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        )
        .await
        .map_err(|e| AppError::Sftp(format!("open remote {temp_path}: {e}")))
    };
    let mut remote_file = match with_timeout("sftp write_bytes open", open_fut).await {
        Ok(f) => f,
        Err(e) => {
            drop(guard);
            session.invalidate_sftp().await;
            return Err(e);
        }
    };
    let write_res = remote_file
        .write_all(bytes)
        .await
        .map_err(|e| AppError::Sftp(format!("write remote {temp_path}: {e}")));
    let _ = remote_file.shutdown().await;
    if let Err(e) = write_res {
        let _ = tokio::time::timeout(SFTP_OP_TIMEOUT, sftp.remove_file(&temp_path)).await;
        return Err(e);
    }
    // chmod 0600 before publishing so the store is never briefly group/world
    // readable. Best-effort: some servers reject SETSTAT — not fatal.
    let _ = with_timeout("sftp write_bytes chmod", async {
        let attrs = FileAttributes {
            permissions: Some(0o600),
            ..Default::default()
        };
        sftp.set_metadata(&temp_path, attrs)
            .await
            .map_err(|e| AppError::Sftp(format!("chmod {temp_path}: {e}")))
    })
    .await;
    // SSH_FXP_RENAME on OpenSSH does NOT overwrite an existing target — it
    // returns failure. The index store writes to a FIXED path (index.json /
    // meta.json) that gets replaced on every re-index. russh-sftp 2.0.8 has no
    // posix-rename extension, so we do a NON-DESTRUCTIVE overwrite: try the
    // direct rename (first-ever write succeeds); if the target exists, MOVE IT
    // ASIDE to a backup, publish the new file, then drop the backup. If
    // publishing fails, restore the backup — the durable store is NEVER left
    // empty.
    //
    // CRITICAL: each SFTP rpc gets its OWN timeout, and the orchestration runs as
    // plain sequential code — NOT wrapped in a single outer `with_timeout`. A
    // single wrapper, on timeout, drops the in-flight future at its current await
    // point, so a hang on the publish rename would skip the restore branch and
    // strand the live store at the backup path. With per-rpc timeouts a hung rpc
    // surfaces as an `Err`, and the restore code below always runs.
    let direct = with_timeout("sftp write_bytes rename", async {
        sftp.rename(&temp_path, remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("rename {temp_path} -> {remote_path}: {e}")))
    })
    .await;
    if direct.is_ok() {
        return Ok(()); // first-ever write (no existing target) — temp consumed.
    }

    let bak = format!("{remote_path}.{}.tersh-bak", uuid::Uuid::new_v4().simple());
    let moved_aside = with_timeout("sftp write_bytes move-aside", async {
        sftp.rename(remote_path, &bak)
            .await
            .map_err(|e| AppError::Sftp(format!("move-aside {remote_path} -> {bak}: {e}")))
    })
    .await;
    if moved_aside.is_err() {
        // Could not move it aside (it may not actually exist now) — one more
        // direct attempt; surface the error if it still fails.
        let retry = with_timeout("sftp write_bytes rename retry", async {
            sftp.rename(&temp_path, remote_path)
                .await
                .map_err(|e| AppError::Sftp(format!("rename {temp_path} -> {remote_path}: {e}")))
        })
        .await;
        if retry.is_err() {
            remove_remote_quietly(sftp, &temp_path).await; // no `.tersh-tmp` orphan
        }
        return retry;
    }

    // Old store is now safe at `bak`. Publish the new file.
    let published = with_timeout("sftp write_bytes publish", async {
        sftp.rename(&temp_path, remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("rename {temp_path} -> {remote_path}: {e}")))
    })
    .await;
    match published {
        Ok(()) => {
            remove_remote_quietly(sftp, &bak).await; // publish succeeded; drop old copy
            Ok(())
        }
        Err(publish_err) => {
            // Publish failed — RESTORE the old store and VERIFY it. The canonical
            // path must never be left empty. If the restore ALSO fails (e.g. a
            // correlated disk-full), the old contents survive at `bak`; surface
            // that path in the error and let read_remote_bytes self-heal from it.
            let restored = with_timeout("sftp write_bytes restore", async {
                sftp.rename(&bak, remote_path)
                    .await
                    .map_err(|e| AppError::Sftp(format!("restore {bak} -> {remote_path}: {e}")))
            })
            .await;
            remove_remote_quietly(sftp, &temp_path).await;
            match restored {
                Ok(()) => Err(publish_err),
                Err(restore_err) => Err(AppError::Sftp(format!(
                    "publish failed ({publish_err}) AND restore failed — previous index \
                     preserved at {bak}: {restore_err}"
                ))),
            }
        }
    }
}

/// Best-effort remove with its own timeout, ignoring the result. Used for
/// dropping temp/backup files where a failure is harmless (a leaked `.tersh-bak`
/// is recovered on read; a leaked `.tersh-tmp` is overwritten next write).
async fn remove_remote_quietly(sftp: &SftpSession, path: &str) {
    let _ = with_timeout("sftp remove", async {
        sftp.remove_file(path)
            .await
            .map_err(|e| AppError::Sftp(e.to_string()))
    })
    .await;
}

/// If `remote_path` is absent but a sibling `<basename>.<uuid>.tersh-bak` exists,
/// a prior write crashed/failed after moving the live store aside but before (or
/// during) restore. Adopt the most-relevant backup back into place so the index
/// isn't silently treated as "never built". Returns true if one was recovered.
async fn recover_tersh_bak(sftp: &SftpSession, remote_path: &str) -> bool {
    let (dir, file) = match remote_path.rsplit_once('/') {
        Some((d, f)) if !f.is_empty() => (if d.is_empty() { "/" } else { d }, f),
        _ => return false,
    };
    let prefix = format!("{file}.");
    let entries = match with_timeout("sftp recover readdir", async {
        sftp.read_dir(dir)
            .await
            .map_err(|e| AppError::Sftp(format!("read_dir {dir}: {e}")))
    })
    .await
    {
        Ok(d) => d,
        Err(_) => return false,
    };
    // Match `<file>.<32-hex-uuid>.tersh-bak` exactly — the shape write_remote_bytes
    // produces (uuid::Uuid::simple()). Requiring the middle to be a 32-char hex
    // uuid avoids adopting an unrelated file's backup if more canonical files are
    // ever added under `.tersh/` (e.g. a hypothetical `index.json.<x>`).
    let bak_name = entries.into_iter().map(|e| e.file_name()).find(|name| {
        name.strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix(".tersh-bak"))
            .is_some_and(|mid| mid.len() == 32 && mid.bytes().all(|b| b.is_ascii_hexdigit()))
    });
    let Some(bak_name) = bak_name else {
        return false;
    };
    let bak_path = if dir.ends_with('/') {
        format!("{dir}{bak_name}")
    } else {
        format!("{dir}/{bak_name}")
    };
    with_timeout("sftp recover rename", async {
        sftp.rename(&bak_path, remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("recover {bak_path} -> {remote_path}: {e}")))
    })
    .await
    .is_ok()
}

/// Read an absolute remote file into memory. Returns Ok(None) if it doesn't
/// exist (so callers can treat "no index on this VPS yet" as a normal state).
pub async fn read_remote_bytes(
    session: &SshSession,
    remote_path: &str,
) -> AppResult<Option<Vec<u8>>> {
    let guard = session.acquire_sftp().await?;
    let sftp = guard.sftp();
    // Absent path → None. metadata() errors when the file doesn't exist. But a
    // crashed/failed write may have stranded the previous contents at a sibling
    // `.tersh-bak`; recover it before declaring the file missing, then re-stat.
    let first_stat = with_timeout("sftp read_bytes stat", async {
        sftp.metadata(remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("stat {remote_path}: {e}")))
    })
    .await;
    let meta = match first_stat {
        Ok(m) => m,
        Err(_) if recover_tersh_bak(sftp, remote_path).await => {
            match with_timeout("sftp read_bytes stat", async {
                sftp.metadata(remote_path)
                    .await
                    .map_err(|e| AppError::Sftp(format!("stat {remote_path}: {e}")))
            })
            .await
            {
                Ok(m) => m,
                Err(_) => return Ok(None),
            }
        }
        Err(_) => return Ok(None),
    };
    if meta.size.unwrap_or(0) > MAX_REMOTE_READ_BYTES {
        return Err(AppError::Sftp(format!(
            "remote file {remote_path} exceeds {MAX_REMOTE_READ_BYTES} byte read limit"
        )));
    }
    let mut file = with_timeout("sftp read_bytes open", async {
        sftp.open(remote_path)
            .await
            .map_err(|e| AppError::Sftp(format!("open remote {remote_path}: {e}")))
    })
    .await?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)
        .await
        .map_err(|e| AppError::Sftp(format!("read remote {remote_path}: {e}")))?;
    let _ = file.shutdown().await;
    Ok(Some(out))
}

async fn mkdir_p(sftp: &SftpSession, path: &str) -> AppResult<()> {
    // Walk path components and create each. If create_dir fails AND the path
    // doesn't already exist as a directory, surface the error — otherwise we
    // silently swallow permission-denied and quota-exceeded, then the upload
    // fails downstream with a confusing "open remote" error.
    let mut accumulated = String::from("/");
    for part in path.trim_start_matches('/').split('/') {
        if part.is_empty() {
            continue;
        }
        if !accumulated.ends_with('/') {
            accumulated.push('/');
        }
        accumulated.push_str(part);
        if sftp.create_dir(&accumulated).await.is_err() {
            // Already-exists is fine; anything else is a real error.
            match sftp.metadata(&accumulated).await {
                Ok(meta) if meta.is_dir() => continue,
                Ok(_) => {
                    return Err(AppError::Sftp(format!(
                        "mkdir {accumulated}: path exists but is not a directory"
                    )));
                }
                Err(e) => {
                    return Err(AppError::Sftp(format!("mkdir {accumulated}: {e}")));
                }
            }
        }
    }
    Ok(())
}

/// Backend-side sensitive-path check. Mirrors the renderer's `pathLooksSensitive`
/// regex list so a compromised renderer can't bypass it. Returns the matched
/// pattern name (for user-facing error text), or None if the path looks OK.
pub fn sensitive_path_reason(path: &str) -> Option<&'static str> {
    let normalized = path
        .split('/')
        .map(|part| part.trim_end())
        .collect::<Vec<_>>()
        .join("/");
    let lower = normalized.to_lowercase();
    if lower.contains("/.ssh/") {
        return Some(".ssh directory");
    }
    if lower.ends_with(".pem") {
        return Some(".pem certificate/key");
    }
    if lower.ends_with(".p12") || lower.ends_with(".pfx") {
        return Some("PKCS#12 keystore");
    }
    if lower.ends_with(".keystore") {
        return Some("keystore file");
    }
    if lower.ends_with(".kdbx") {
        return Some("KeePass database");
    }
    if lower.ends_with(".key") {
        return Some("private key");
    }
    if lower.ends_with(".1pux") {
        return Some("1Password export");
    }
    if lower.ends_with("/secrets.json") || lower.ends_with("/secret.json") {
        return Some("secrets file");
    }
    if lower.contains("/.aws/credentials") {
        return Some("AWS credentials");
    }
    if lower.contains("/.kube/config") {
        return Some("kubeconfig");
    }
    if lower.contains("/.config/gh/") {
        return Some("gh CLI config");
    }
    if lower.contains("/.config/gcloud/") {
        return Some("gcloud config");
    }
    if lower.ends_with("/.env") || lower.contains("/.env.") {
        return Some(".env file");
    }
    // OpenSSH private-key naming pattern
    let basename = std::path::Path::new(&normalized)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim_end();
    if matches!(
        basename,
        "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "id_rsa.pub"
            | "id_dsa.pub"
            | "id_ecdsa.pub"
            | "id_ed25519.pub"
    ) {
        return Some("OpenSSH key");
    }
    if basename.starts_with("id_rsa")
        || basename.starts_with("id_dsa")
        || basename.starts_with("id_ecdsa")
        || basename.starts_with("id_ed25519")
    {
        return Some("OpenSSH key");
    }
    if basename.starts_with("wallet.") {
        return Some("crypto wallet file");
    }
    None
}

/// Strip anything that isn't `[A-Za-z0-9._-]`, collapse to underscore.
/// Prevents path traversal and weird unicode tricks in user-supplied names.
fn sanitize_basename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // never allow leading dot beyond a single one, never empty
    if out.is_empty() {
        out.push_str("file");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{join_home_path, sanitize_basename, sensitive_path_reason};

    #[test]
    fn sensitive_path_detection_catches_common_secret_files() {
        assert_eq!(
            sensitive_path_reason("/Users/me/.ssh/id_ed25519"),
            Some(".ssh directory")
        );
        assert_eq!(
            sensitive_path_reason("/tmp/prod.pem"),
            Some(".pem certificate/key")
        );
        assert_eq!(
            sensitive_path_reason("/Users/me/.aws/credentials"),
            Some("AWS credentials")
        );
        assert_eq!(
            sensitive_path_reason("/work/app/.env.local"),
            Some(".env file")
        );
        assert_eq!(
            sensitive_path_reason("/tmp/wallet.dat"),
            Some("crypto wallet file")
        );
        assert_eq!(sensitive_path_reason("/tmp/screenshot.png"), None);
    }

    #[test]
    fn sanitize_basename_removes_path_traversal_and_shell_chars() {
        assert_eq!(sanitize_basename("../id_rsa; rm -rf"), ".._id_rsa__rm_-rf");
        assert_eq!(sanitize_basename(""), "file");
        assert_eq!(sanitize_basename("screen shot.png"), "screen_shot.png");
    }

    #[test]
    fn join_home_path_expands_tilde_children_without_double_slashes() {
        assert_eq!(join_home_path("/root", ".ssh"), "/root/.ssh");
        assert_eq!(join_home_path("/root/", "logs/app"), "/root/logs/app");
        assert_eq!(join_home_path("/", "var"), "/var");
        assert_eq!(join_home_path("/home/user", ""), "/home/user");
    }
}
