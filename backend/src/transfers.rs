use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Tracks in-flight SFTP transfers so the UI can cancel them. The flag lives
/// at the granularity of one transfer (one upload or one download); the loop
/// inside `sftp::upload_to_path` / `download_to_path` checks `is_cancelled()`
/// between chunks and bails out cleanly (rolling back partial writes via the
/// temp-file pattern for uploads).
pub struct TransferRegistry {
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TransferRegistry {
    pub fn new() -> Self {
        Self {
            cancels: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new transfer. Returns the shared cancel flag the
    /// transfer's worker should check on each chunk. The same flag can be
    /// flipped by `cancel(id)` from any Tauri command.
    pub async fn register(&self, transfer_id: String) -> Arc<AtomicBool> {
        let mut guard = self.cancels.lock().await;
        let flag = Arc::new(AtomicBool::new(false));
        guard.insert(transfer_id, flag.clone());
        flag
    }

    pub async fn cancel(&self, transfer_id: &str) -> bool {
        if let Some(flag) = self.cancels.lock().await.get(transfer_id) {
            flag.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub async fn unregister(&self, transfer_id: &str) {
        self.cancels.lock().await.remove(transfer_id);
    }
}
