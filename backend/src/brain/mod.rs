use crate::errors::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod embed;
pub mod index;
pub mod lineage;
pub mod ngram;
pub mod resolve;
pub mod secrets;
pub mod storage;
pub mod tools;
pub mod watcher;

/// Stable, content-addressed identifier for a brain. Computed by FNV-1a 64
/// over the scope key string, hex-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BrainId(pub String);

impl BrainId {
    pub fn from_scope_key(key: &str) -> Self {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in key.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        BrainId(format!("{hash:016x}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainScope {
    Local {
        root: PathBuf,
    },
    Remote {
        host_id: String,
        host_fingerprint: String,
        remote_root: String,
    },
}

impl BrainScope {
    pub fn scope_key(&self) -> String {
        match self {
            BrainScope::Local { root } => format!("local:{}", root.display()),
            BrainScope::Remote {
                host_fingerprint,
                remote_root,
                ..
            } => format!("remote:{host_fingerprint}:{remote_root}"),
        }
    }

    pub fn label(&self) -> String {
        match self {
            BrainScope::Local { root } => root
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("project")
                .to_string(),
            BrainScope::Remote { remote_root, .. } => remote_root
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("project")
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainMeta {
    pub id: BrainId,
    pub label: String,
    pub scope: BrainScope,
    pub created_at: i64,
    pub last_used_at: i64,
}

impl BrainMeta {
    pub fn new(scope: BrainScope) -> Self {
        let id = BrainId::from_scope_key(&scope.scope_key());
        let now = unix_secs();
        let label = scope.label();
        Self {
            id,
            label,
            scope,
            created_at: now,
            last_used_at: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainStatus {
    pub id: BrainId,
    pub label: String,
    pub scope: BrainScope,
    pub last_used_at: i64,
    pub indexed_at: i64,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub overview: String,
    pub project_digest: String,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub capabilities: Vec<String>,
    pub architecture: Vec<String>,
    pub modules: Vec<String>,
    /// True when an embeddings layer is present in the index.
    #[serde(default)]
    pub has_embeddings: bool,
    /// Unix timestamp when embeddings first went stale, or null when
    /// embeddings are absent / fresh.
    #[serde(default)]
    pub embeddings_stale_since: Option<i64>,
    /// Embedding model recorded on the persisted store ("" when none).
    #[serde(default)]
    pub embedding_model: String,
}

pub struct BrainHandle {
    pub meta: Mutex<BrainMeta>,
}

impl BrainHandle {
    pub fn new(meta: BrainMeta) -> Self {
        Self {
            meta: Mutex::new(meta),
        }
    }
}

pub struct BrainRegistry {
    inner: Mutex<HashMap<BrainId, Arc<BrainHandle>>>,
    /// Polling file watchers, one per enabled local brain. Dropped when
    /// the brain is disabled or the registry is torn down.
    watchers: Mutex<HashMap<BrainId, watcher::WatcherHandle>>,
    /// Brains with a refresh/build in flight. SHARED across every refresh entry
    /// point (reconnect-resync, manual Refresh, on-use expiry) and keyed by
    /// BrainId (which is host+root, session-independent) so two paths or two
    /// tabs can never run build_remote concurrently on the same `<root>/.tersh`.
    /// A std Mutex (not tokio): the lock is only ever held for a HashSet
    /// insert/remove, never across an await, and a std lock lets the RAII
    /// `RefreshGuard` release the slot from Drop without an async context.
    refreshing: std::sync::Mutex<HashSet<BrainId>>,
}

impl BrainRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            refreshing: std::sync::Mutex::new(HashSet::new()),
        }
    }

    /// Claim the refresh slot for a brain. Returns `Some(guard)` if claimed; the
    /// slot is released when the guard drops, so a panic or a dropped future
    /// inside build_remote/persist_remote can't leak the slot and permanently
    /// wedge every future refresh of that brain (the old manual `end_refresh`
    /// pair was skipped on unwind). Returns `None` if a refresh is already in
    /// flight for this brain.
    #[must_use = "dropping the guard releases the refresh slot; hold it for the build's lifetime"]
    pub fn begin_refresh(self: &Arc<Self>, id: &BrainId) -> Option<RefreshGuard> {
        let claimed = self
            .refreshing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone());
        claimed.then(|| RefreshGuard {
            registry: Arc::clone(self),
            id: id.clone(),
        })
    }

    /// Load any brains already on disk so they survive app restarts.
    pub async fn restore_from_disk(self: &Arc<Self>) -> AppResult<()> {
        let root = storage::brain_root_dir()?;
        if !root.exists() {
            return Ok(());
        }
        let mut rd = tokio::fs::read_dir(&root)
            .await
            .map_err(|e| AppError::Internal(format!("read brain root: {e}")))?;
        let mut count = 0;
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| AppError::Internal(format!("read brain entry: {e}")))?
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let bytes = match tokio::fs::read(&meta_path).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("brain meta unreadable {}: {e}", meta_path.display());
                    continue;
                }
            };
            let meta: BrainMeta = match serde_json::from_slice(&bytes) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("brain meta parse {}: {e}", meta_path.display());
                    continue;
                }
            };
            // Only LOCAL brains are restored from the Mac. Remote brains now
            // live on the VPS and are hydrated over SFTP when their host
            // connects — any remote meta.json left on disk here is a pre-
            // migration leftover and is ignored.
            if !matches!(meta.scope, BrainScope::Local { .. }) {
                continue;
            }
            let id = meta.id.clone();
            self.inner
                .lock()
                .await
                .insert(id.clone(), Arc::new(BrainHandle::new(meta)));
            // Resume the polling watcher for any restored local brain so
            // refresh stays continuous across app restarts.
            let handle = watcher::spawn(self.clone(), id.clone());
            self.watchers.lock().await.insert(id, handle);
            count += 1;
        }
        if count > 0 {
            tracing::info!("brain registry restored {count} brain(s)");
        }
        Ok(())
    }

    pub async fn get(&self, id: &BrainId) -> Option<Arc<BrainHandle>> {
        self.inner.lock().await.get(id).cloned()
    }

    pub async fn get_by_scope(&self, scope: &BrainScope) -> Option<Arc<BrainHandle>> {
        let id = BrainId::from_scope_key(&scope.scope_key());
        self.get(&id).await
    }

    pub async fn list(&self) -> Vec<BrainStatus> {
        let guard = self.inner.lock().await;
        let mut out = Vec::with_capacity(guard.len());
        for handle in guard.values() {
            let meta = handle.meta.lock().await;
            out.push(BrainStatus {
                id: meta.id.clone(),
                label: meta.label.clone(),
                scope: meta.scope.clone(),
                last_used_at: meta.last_used_at,
                indexed_at: 0,
                files_indexed: 0,
                chunks_indexed: 0,
                overview: String::new(),
                project_digest: String::new(),
                languages: Vec::new(),
                frameworks: Vec::new(),
                capabilities: Vec::new(),
                architecture: Vec::new(),
                modules: Vec::new(),
                has_embeddings: false,
                embeddings_stale_since: None,
                embedding_model: String::new(),
            });
        }
        drop(guard);
        for status in &mut out {
            if let Some(stats) = index::stats(&status.id).await {
                status.indexed_at = stats.indexed_at;
                status.files_indexed = stats.files_indexed;
                status.chunks_indexed = stats.chunks_indexed;
                status.overview = stats.overview;
                status.project_digest = stats.project_digest;
                status.languages = stats.languages;
                status.frameworks = stats.frameworks;
                status.capabilities = stats.capabilities;
                status.architecture = stats.architecture;
                status.modules = stats.modules;
                status.has_embeddings = stats.has_embeddings;
                status.embeddings_stale_since = stats.embeddings_stale_since;
                status.embedding_model = stats.embedding_model;
            }
        }
        out
    }

    pub async fn enable_local(self: &Arc<Self>, root: PathBuf) -> AppResult<BrainId> {
        let canonical = std::fs::canonicalize(&root)
            .map_err(|e| AppError::Invalid(format!("project root unavailable: {e}")))?;
        if !canonical.is_dir() {
            return Err(AppError::Invalid("brain root must be a directory".into()));
        }
        let scope = BrainScope::Local { root: canonical };
        self.enable_scope(scope).await
    }

    pub async fn enable_remote(
        self: &Arc<Self>,
        host_id: String,
        host_fingerprint: String,
        remote_root: String,
    ) -> AppResult<BrainId> {
        if !remote_root.starts_with('/') {
            return Err(AppError::Invalid(
                "remote root must be an absolute path".into(),
            ));
        }
        let scope = BrainScope::Remote {
            host_id,
            host_fingerprint,
            remote_root,
        };
        self.enable_scope(scope).await
    }

    async fn enable_scope(self: &Arc<Self>, scope: BrainScope) -> AppResult<BrainId> {
        let meta = BrainMeta::new(scope.clone());
        if let BrainScope::Local { root } = &meta.scope {
            let index = index::build_local(meta.clone(), root.clone(), None).await?;
            index::write_index(&index).await?;
        }
        self.register_scope(scope).await
    }

    pub async fn register_scope(self: &Arc<Self>, scope: BrainScope) -> AppResult<BrainId> {
        let meta = BrainMeta::new(scope.clone());
        let id = meta.id.clone();
        // Only LOCAL brains persist their meta to the Mac. Remote brains live on
        // the VPS (~/.tersh/brain); their meta.json is pushed over SFTP by
        // index::persist_remote, so nothing about a remote project touches the
        // Mac's disk.
        if matches!(scope, BrainScope::Local { .. }) {
            let dir = storage::brain_dir(&id)?;
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| AppError::Internal(format!("create brain dir: {e}")))?;
            storage::write_meta(&meta).await?;
        }
        let handle = Arc::new(BrainHandle::new(meta));
        self.inner.lock().await.insert(id.clone(), handle);
        // Spawn the polling watcher for local brains. Remote brains skip
        // the watcher (no credential bridge yet).
        if matches!(scope, BrainScope::Local { .. }) {
            let handle = watcher::spawn(self.clone(), id.clone());
            self.watchers.lock().await.insert(id.clone(), handle);
        }
        Ok(id)
    }

    /// Insert a brain hydrated from the VPS — meta + index already downloaded
    /// over SFTP. Used when a remote host connects and we restore its
    /// `~/.tersh/brain` store into RAM. Idempotent: re-hydrating replaces the
    /// in-memory copy.
    pub async fn hydrate_remote(self: &Arc<Self>, meta: BrainMeta, index: index::ProjectIndex) {
        let id = meta.id.clone();
        index::cache_put_remote(index).await;
        self.inner
            .lock()
            .await
            .insert(id, Arc::new(BrainHandle::new(meta)));
    }

    pub async fn disable(self: &Arc<Self>, id: &BrainId) -> AppResult<()> {
        if let Some(handle) = self.watchers.lock().await.remove(id) {
            handle.abort();
        }
        self.inner.lock().await.remove(id);
        // Drop any RAM-cached remote index. (Deleting the VPS copy needs an SSH
        // session and is handled at the command layer.)
        index::cache_remove_remote(id).await;
        let dir = storage::brain_dir(id)?;
        if dir.exists() {
            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!("remove brain dir {}: {e}", dir.display());
            }
        }
        Ok(())
    }

    pub async fn refresh(self: &Arc<Self>, id: &BrainId) -> AppResult<()> {
        let handle = self
            .get(id)
            .await
            .ok_or_else(|| AppError::Invalid("project index not found".into()))?;
        let meta = handle.meta.lock().await.clone();
        match meta.scope.clone() {
            BrainScope::Local { root } => {
                let index = index::build_local(meta, root, None).await?;
                index::write_index(&index).await?;
                Ok(())
            }
            BrainScope::Remote { .. } => Err(AppError::Invalid(
                "remote project index refresh is not available yet".into(),
            )),
        }
    }

    pub async fn refresh_if_expired(self: &Arc<Self>, scope: &BrainScope) -> AppResult<bool> {
        let id = BrainId::from_scope_key(&scope.scope_key());
        let handle = match self.get(&id).await {
            Some(handle) => handle,
            None => return Ok(false),
        };
        if !index::is_expired(&id, index::AUTO_REFRESH_AFTER_SECS).await? {
            return Ok(false);
        }

        let meta = handle.meta.lock().await.clone();
        match meta.scope.clone() {
            BrainScope::Local { root } => {
                let project_index = index::build_local(meta, root, None).await?;
                index::write_index(&project_index).await?;
                Ok(true)
            }
            BrainScope::Remote { .. } => Ok(false),
        }
    }

    pub async fn touch_used(&self, id: &BrainId) {
        if let Some(handle) = self.get(id).await {
            let mut meta = handle.meta.lock().await;
            meta.last_used_at = unix_secs();
            // Only local brains write meta to the Mac. Remote brains' durable
            // meta lives on the VPS; last_used is updated in RAM only (we don't
            // open an SSH session just to bump a timestamp).
            if matches!(meta.scope, BrainScope::Local { .. }) {
                let _ = storage::write_meta(&meta).await;
            }
        }
    }
}

/// RAII claim on a brain's refresh slot, returned by
/// [`BrainRegistry::begin_refresh`]. The slot is released on `Drop` — on a
/// normal scope exit, an early `?`-return, OR an unwind from a panic inside the
/// build — so the in-flight set can't be permanently wedged.
pub struct RefreshGuard {
    registry: Arc<BrainRegistry>,
    id: BrainId,
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        self.registry
            .refreshing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.id);
    }
}

pub fn unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
