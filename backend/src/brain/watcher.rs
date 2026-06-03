//! Project Index file watcher.
//!
//! v1 ships as a polling watcher (zero new deps) — wakes every
//! `POLL_INTERVAL_SECS` and triggers `BrainRegistry::refresh` when any
//! tracked file under the project root has an mtime later than the
//! current `indexed_at`. Cheap on small/medium projects (5-50 ms scan
//! for a few thousand files).
//!
//! Upgrade path: swap the polling loop for an event-driven
//! `notify::RecommendedWatcher` once that crate clears the §2.1 review
//! in `docs/security/allowed-deps.md`. Public API stays identical.

use crate::brain::{index, secrets, BrainId, BrainRegistry, BrainScope};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// How often the watcher checks for changed files. 30s is responsive
/// enough for "save → Ctrl+P" UX without burning CPU on idle projects.
pub const POLL_INTERVAL_SECS: u64 = 30;
/// Force a full refresh at most once an hour even if the watcher saw no
/// changes — catches the case where mtime granularity or rewrite-in-place
/// tools missed an update.
pub const FORCE_REFRESH_SECS: i64 = 60 * 60;
/// Maximum files we'll stat per scan. Keeps the watcher bounded on huge
/// monorepos without sacrificing correctness on normal projects.
const MAX_SCAN_FILES: usize = 4_000;

pub async fn run_polling_watcher(
    registry: Arc<BrainRegistry>,
    id: BrainId,
    cancel: Arc<AtomicBool>,
) {
    let mut last_full_refresh: i64 = crate::brain::unix_secs();
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
        if cancel.load(Ordering::Acquire) {
            break;
        }

        let scope = match registry.get(&id).await {
            Some(handle) => handle.meta.lock().await.scope.clone(),
            None => break,
        };
        let root = match scope {
            BrainScope::Local { root } => root,
            BrainScope::Remote { .. } => {
                // Remote watching needs SSH access; defer to manual
                // refresh until we have a credential bridge.
                continue;
            }
        };

        let indexed_at = match index::load_index(&id).await {
            Ok(Some(idx)) => idx.indexed_at,
            _ => 0,
        };
        let now = crate::brain::unix_secs();
        let due_for_force = now.saturating_sub(last_full_refresh) >= FORCE_REFRESH_SECS;
        let changed = if due_for_force {
            true
        } else {
            project_has_changes_since(&root, indexed_at).await
        };
        if !changed {
            continue;
        }
        match registry.refresh(&id).await {
            Ok(()) => {
                last_full_refresh = now;
                tracing::debug!(brain = %id.0, "project index refreshed by watcher");
            }
            Err(err) => {
                tracing::warn!(brain = %id.0, "watcher refresh failed: {err}");
            }
        }
    }
}

/// Returns true if any tracked file under `root` has an mtime newer
/// than `since`. Respects the same path-skip rules as the indexer so
/// we don't trigger on changes to `node_modules`, `.git`, etc.
async fn project_has_changes_since(root: &Path, since: i64) -> bool {
    let root_for_blocking = root.to_path_buf();
    let result =
        tokio::task::spawn_blocking(move || scan_for_changes(&root_for_blocking, since)).await;
    matches!(result, Ok(true))
}

fn scan_for_changes(root: &Path, since: i64) -> bool {
    let mut stack = vec![root.to_path_buf()];
    let mut count = 0_usize;
    while let Some(dir) = stack.pop() {
        if count >= MAX_SCAN_FILES {
            return true; // Bail out conservatively — assume a giant repo changed.
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            count += 1;
            if count >= MAX_SCAN_FILES {
                return true;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name != ".github" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                // Skip the same noisy dirs the indexer skips.
                if matches!(
                    name.as_str(),
                    "node_modules"
                        | "target"
                        | "dist"
                        | "build"
                        | ".next"
                        | ".turbo"
                        | ".git"
                        | "vendor"
                        | "coverage"
                        | "__pycache__"
                        | ".venv"
                        | ".cache"
                ) {
                    continue;
                }
                let path_str = path.to_string_lossy();
                if secrets::path_skip_reason(&format!("/{path_str}/")).is_some() {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if mtime > since {
                    return true;
                }
            }
        }
    }
    false
}

/// Spawn the watcher and return its abort handle. Stored on the
/// BrainRegistry so disable() can stop it cleanly.
pub fn spawn(registry: Arc<BrainRegistry>, id: BrainId) -> WatcherHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_task = cancel.clone();
    let join = tokio::spawn(async move {
        run_polling_watcher(registry, id, cancel_for_task).await;
    });
    WatcherHandle { cancel, join }
}

/// Owned handle to a running watcher. Drop the handle (or call abort)
/// to stop the loop.
pub struct WatcherHandle {
    cancel: Arc<AtomicBool>,
    join: tokio::task::JoinHandle<()>,
}

impl WatcherHandle {
    pub fn abort(&self) {
        self.cancel.store(true, Ordering::Release);
        self.join.abort();
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.join.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fresh_temp_dir(slug: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "tersh-watcher-test-{slug}-{}-{}",
            std::process::id(),
            crate::brain::unix_secs()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scan_returns_false_when_nothing_changed_since_future() {
        let dir = fresh_temp_dir("future");
        fs::write(dir.join("a.rs"), "fn main() {}").unwrap();
        // Compare against a timestamp far in the future — nothing can be newer.
        let future = crate::brain::unix_secs() + 60 * 60 * 24;
        assert!(!scan_for_changes(&dir, future));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_returns_true_when_a_file_was_just_modified() {
        let dir = fresh_temp_dir("now");
        fs::write(dir.join("a.rs"), "fn main() {}").unwrap();
        // Compare against the unix epoch — every real file is newer.
        assert!(scan_for_changes(&dir, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_skips_node_modules_and_other_noisy_dirs() {
        let dir = fresh_temp_dir("skip");
        let noisy = dir.join("node_modules");
        fs::create_dir_all(&noisy).unwrap();
        fs::write(noisy.join("bigblob.js"), "throw 1").unwrap();
        // Root has nothing else — the only changed file is inside
        // node_modules, which the scanner should skip.
        assert!(!scan_for_changes(&dir, 0));
        let _ = fs::remove_dir_all(&dir);
    }
}
