use crate::errors::{AppError, AppResult};
use crate::ssh::SshSession;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// In-memory registry of live SSH sessions, keyed by session id.
/// Each session owns its tokio task that pumps the channel.
pub struct SessionRegistry {
    inner: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    host_id: String,
    session: Arc<SshSession>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub async fn insert(&self, host_id: String, session: SshSession) -> String {
        let id = session.id.clone();
        self.inner.lock().await.insert(
            id.clone(),
            Entry {
                host_id,
                session: Arc::new(session),
            },
        );
        id
    }

    pub async fn get(&self, id: &str) -> AppResult<Arc<SshSession>> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|e| e.session.clone())
            .ok_or_else(|| AppError::SessionNotFound(id.to_string()))
    }

    pub async fn host_id_for_session(&self, id: &str) -> AppResult<String> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|e| e.host_id.clone())
            .ok_or_else(|| AppError::SessionNotFound(id.to_string()))
    }

    pub async fn remove(&self, id: &str) -> Option<Arc<SshSession>> {
        self.inner.lock().await.remove(id).map(|e| e.session)
    }

    pub async fn list_for_host(&self, host_id: &str) -> Vec<Arc<SshSession>> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| e.host_id == host_id)
            .map(|e| e.session.clone())
            .collect()
    }
}
