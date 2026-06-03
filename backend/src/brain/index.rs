use crate::brain::{
    embed::{self, EmbeddingConfig},
    lineage::{self, Lineage},
    ngram, resolve, secrets, storage, BrainId, BrainMeta,
};
use crate::errors::{AppError, AppResult};
use crate::ssh::SshSession;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Emitter};

const MAX_INDEX_FILES: usize = 2_000;
const MAX_REMOTE_INDEX_FILES: usize = 400;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_TOTAL_BYTES: u64 = 12 * 1024 * 1024;
const MAX_REMOTE_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
const CHUNK_LINES: usize = 90;
const CHUNK_OVERLAP: usize = 15;
const MAX_CHUNK_CHARS: usize = 8_000;
const MAX_CONTEXT_CHARS: usize = 14_000;
pub const AUTO_REFRESH_AFTER_SECS: i64 = 60 * 60;
/// Shorter window for the auto re-sync that fires when a project reconnects, so
/// a project feels current on login without re-reading the VPS on every rapid
/// reconnect. Gated on the index's `indexed_at` age (not a per-session flag).
pub const RECONNECT_REFRESH_AFTER_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIndex {
    pub id: BrainId,
    pub label: String,
    pub indexed_at: i64,
    pub root_hint: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub total_bytes: u64,
    pub overview: String,
    #[serde(default)]
    pub project_digest: String,
    #[serde(default)]
    pub project_map: ProjectMap,
    pub files: Vec<IndexedFile>,
    /// TF-IDF corpus statistics for the indexed files. Empty for older
    /// index.json files; `score_file_v2` falls back to the legacy
    /// `score_file` heuristic when this is empty.
    #[serde(default)]
    pub corpus_stats: CorpusStats,
    /// Parse-built dependency graph keyed by project-relative file paths.
    /// Edges go from importer → imported. Reverse-adjacency lets us
    /// answer "who imports this file" in O(1).
    #[serde(default)]
    pub graph: DependencyGraph,
    /// Optional git lineage layer. None for non-git projects.
    #[serde(default)]
    pub lineage: Option<Lineage>,
    /// Optional embedding store. Present only when the user supplied an
    /// embedding model at index time. Absent → retrieval falls back to
    /// TF-IDF + n-gram + heuristic.
    #[serde(default)]
    pub embeddings: Option<EmbeddingStore>,
}

/// One embedding vector per file (computed from the file's summary
/// concatenated with its symbols/imports). Per-chunk embeddings are
/// expensive at scale, so v1 sticks to file-level vectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingStore {
    /// Model name used to compute these vectors — recorded so a refresh
    /// with a different model triggers re-embedding.
    pub model: String,
    /// Provider used (informational; helps the UI hint cost).
    pub provider: String,
    /// path → vector. Files without an embedding (errors, skipped)
    /// don't appear here; retrieval gracefully degrades for those.
    pub file_vectors: HashMap<String, Vec<f32>>,
    /// Unix timestamp at which these vectors first became stale —
    /// set the first time a silent refresh (watcher / expiry) couldn't
    /// re-embed because no AI config was in memory. None means "fresh
    /// against the current source." Surfaced through `BrainStatus` so
    /// the UI can prompt the user to do a manual refresh with AI
    /// credentials.
    #[serde(default)]
    pub stale_since: Option<i64>,
}

/// Per-corpus token statistics for TF-IDF scoring. Built once at index
/// time and persisted alongside the index so retrieval is a pure
/// in-memory operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorpusStats {
    /// term → IDF (log((N + 1) / (df + 1)) + 1). Smoothed so a never-seen
    /// term still has a defined idf when the corpus is small.
    pub idf: HashMap<String, f32>,
    /// Document count at index time. Used by retrieval-side IDF lookups
    /// for tokens not present in the corpus.
    pub doc_count: usize,
}

/// Dependency graph derived from per-language import statements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyGraph {
    /// importer_path → list of project-relative paths it imports.
    pub edges: HashMap<String, Vec<String>>,
    /// imported_path → list of project-relative paths that import it.
    pub reverse: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    /// 1-hop neighbours of `path` in either direction.
    pub fn neighbours(&self, path: &str) -> impl Iterator<Item = &String> {
        self.edges
            .get(path)
            .into_iter()
            .flatten()
            .chain(self.reverse.get(path).into_iter().flatten())
    }
}

#[derive(Debug, Clone)]
pub struct RetrievedContext {
    pub text: String,
    pub trace: Vec<RetrievedContextTraceItem>,
}

#[derive(Debug, Clone)]
pub struct RetrievedContextTraceItem {
    pub tool: String,
    pub target: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedFile {
    pub path: String,
    pub language: String,
    pub size: u64,
    pub modified: i64,
    /// Stable content hash for incremental indexing. Empty for index.json
    /// files created before this field existed; unchanged detection then
    /// falls back to size + mtime for one refresh.
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub imports: Vec<String>,
    pub symbols: Vec<String>,
    pub summary: String,
    pub chunks: Vec<IndexChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexChunk {
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectMap {
    pub package_managers: Vec<String>,
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub architecture: Vec<String>,
    #[serde(default)]
    pub modules: Vec<String>,
    pub manifests: Vec<String>,
    pub entrypoints: Vec<String>,
    pub config_files: Vec<String>,
    pub test_files: Vec<String>,
    pub doc_files: Vec<String>,
    #[serde(default)]
    pub scripts: Vec<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
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
    /// True if the index has an embeddings layer at all.
    pub has_embeddings: bool,
    /// Unix timestamp when embeddings first went stale (a silent
    /// refresh couldn't recompute because no AI config was in memory).
    /// None means embeddings are absent or fresh.
    pub embeddings_stale_since: Option<i64>,
    /// Embedding model recorded on the persisted store. Empty string
    /// when no embeddings are present.
    pub embedding_model: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptIntentKind {
    FreshBuild,
    RepoChange,
    BugFix,
    Planning,
    Research,
    Question,
    Unclear,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptContextDecision {
    pub kind: PromptIntentKind,
    pub use_project_context: bool,
    pub reason: String,
}

pub async fn build_local(
    meta: BrainMeta,
    root: PathBuf,
    ai: Option<&EmbeddingConfig>,
) -> AppResult<ProjectIndex> {
    // Git lineage runs through async tokio::process, so collect it
    // separately and stitch it onto the blocking-built index.
    let lineage_result = lineage::collect_git_lineage(&root).await.ok().flatten();
    let root_for_task = root.clone();
    let meta_for_task = meta.clone();
    let id_for_carryover = meta.id.clone();
    let prior_index = load_index(&id_for_carryover).await.ok().flatten();
    let mut index = tokio::task::spawn_blocking(move || {
        build_local_blocking(meta_for_task, root_for_task, prior_index.as_ref())
    })
    .await
    .map_err(|e| AppError::Internal(format!("index task failed: {e}")))??;
    index.lineage = lineage_result;
    if let Some(ai) = ai {
        // User explicitly paid for embeddings on this pass. If the
        // provider call fails, BUBBLE the error rather than silently
        // succeeding with stale vectors — the caller (brain_enable_local
        // / brain_refresh) propagates this to the UI so the user knows
        // their embedding refresh actually failed. Stale vectors stay
        // on disk untouched until the next successful refresh.
        let prior = load_index(&id_for_carryover).await.ok().flatten();
        let store = embed_files(ai, &index.files, prior.as_ref()).await?;
        index.embeddings = Some(store);
    } else {
        // Silent refresh path (watcher / expiry / restore): no AI
        // credentials in memory. Preserve the previous embedding
        // vectors so retrieval doesn't lose its semantic layer between
        // user-initiated refreshes. Vectors are stamped stale_since=now
        // so the UI can prompt for a manual refresh with AI config.
        index.embeddings = carry_over_embeddings(&id_for_carryover, &index.files).await;
    }
    Ok(index)
}

/// Load the prior persisted index for `id` (if any) and carry its
/// embedding vectors forward, keeping only vectors whose file paths
/// still exist in `new_files`. Result is stamped `stale_since=now` so
/// callers and UI can flag the layer as needing a manual refresh.
///
/// Returns None when there's no prior index, or its embeddings field
/// was empty / None to begin with.
async fn carry_over_embeddings(id: &BrainId, new_files: &[IndexedFile]) -> Option<EmbeddingStore> {
    let prior = load_index(id).await.ok().flatten()?;
    let prior_hash: std::collections::HashMap<&str, &str> = prior
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.content_hash.as_str()))
        .collect();
    let new_hash: std::collections::HashMap<&str, &str> = new_files
        .iter()
        .map(|f| (f.path.as_str(), f.content_hash.as_str()))
        .collect();
    let mut store = prior.embeddings?;
    // Keep a vector ONLY when the file still exists AND its content is
    // unchanged. A changed file's old vector describes stale content, so serving
    // it corrupts semantic recall — drop it (it'll be re-embedded on a keyed
    // refresh). Removed files are dropped too.
    store.file_vectors.retain(|path, _| {
        match (new_hash.get(path.as_str()), prior_hash.get(path.as_str())) {
            (Some(n), Some(p)) => n == p,
            _ => false,
        }
    });
    if store.file_vectors.is_empty() {
        return None;
    }
    // Only flag the layer stale if some indexed file now LACKS a kept vector
    // (a changed/new file with no fresh embedding). If every file is still
    // covered by an unchanged-content vector, don't gratuitously demote a
    // previously-fresh index to "stale".
    let any_uncovered = new_files
        .iter()
        .any(|f| !store.file_vectors.contains_key(&f.path));
    store.stale_since = if any_uncovered {
        Some(store.stale_since.unwrap_or_else(crate::brain::unix_secs))
    } else {
        None
    };
    Some(store)
}

/// Live indexing progress, emitted over `brain://index/{id}/progress` while a
/// remote project is being indexed. Internal IPC only — carries counts + the
/// relative path being scanned, never file contents. Mirrors the SFTP
/// transfer-progress event shape. `id` is whatever the renderer subscribed to:
/// a pre-allocated index id for the enable flow, or the real brain id on refresh.
#[derive(Serialize, Clone)]
pub struct IndexProgress {
    pub id: String,
    pub path: String,
    pub processed: usize,
    pub total: usize,
    pub done: bool,
}

/// Best-effort emit. No-op when there's no subscriber tuple; emit failure must
/// never abort indexing.
fn emit_index_progress(
    progress: Option<&(AppHandle, String)>,
    path: &str,
    processed: usize,
    total: usize,
    done: bool,
) {
    let Some((app, id)) = progress else { return };
    let payload = IndexProgress {
        id: id.clone(),
        path: path.to_string(),
        processed,
        total,
        done,
    };
    let _ = app.emit(&format!("brain://index/{id}/progress"), payload);
}

pub async fn build_remote(
    meta: BrainMeta,
    session: Arc<SshSession>,
    remote_root: String,
    ai: Option<&EmbeddingConfig>,
    progress: Option<(AppHandle, String)>,
) -> AppResult<ProjectIndex> {
    let candidates = collect_remote_files(&session, &remote_root).await?;
    let prior_index = load_index(&meta.id).await.ok().flatten();
    let prior_files = prior_file_map(prior_index.as_ref());
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;

    // Denominator captured before the loop consumes `candidates`. `processed`
    // is a monotonic counter (incremented even on skipped/over-cap files) so
    // the percentage advances smoothly and never overshoots.
    let total = candidates.len();
    let mut processed = 0usize;

    for candidate in candidates {
        processed += 1;
        // Throttle: first file, then every 8th, plus a guaranteed final emit
        // after the loop. Emitting per-file would flood the IPC bridge.
        if processed == 1 || processed % 8 == 0 {
            emit_index_progress(progress.as_ref(), &candidate.rel, processed, total, false);
        }
        if files.len() >= MAX_REMOTE_INDEX_FILES || total_bytes >= MAX_REMOTE_TOTAL_BYTES {
            break;
        }
        if candidate.size > MAX_FILE_BYTES {
            continue;
        }
        if secrets::path_skip_reason(&candidate.path).is_some()
            || !looks_like_source_or_doc(Path::new(&candidate.rel))
        {
            continue;
        }

        let cmd = format!(
            "head -c {cap} -- {path_q} 2>/dev/null",
            cap = MAX_FILE_BYTES,
            path_q = shell_quote(&candidate.path)
        );
        let bytes = match session.exec_oneshot(&cmd, MAX_FILE_BYTES as usize).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(path = %candidate.path, "remote index read skipped: {e}");
                continue;
            }
        };
        if secrets::content_secret_kind(&bytes).is_some() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if text.contains('\0') {
            continue;
        }
        let content_hash = content_hash(&bytes);
        total_bytes = total_bytes.saturating_add(candidate.size);
        files.push(index_text_file(
            &candidate.rel,
            candidate.size,
            candidate.modified,
            text,
            content_hash,
            prior_files.get(candidate.rel.as_str()).copied(),
        ));
    }

    emit_index_progress(progress.as_ref(), "", processed, total, true);

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let chunks_indexed = files.iter().map(|f| f.chunks.len()).sum();
    let project_map = build_project_map(&files);
    let overview = build_overview(&meta.label, &project_map, &files);
    let project_digest = build_project_digest(&meta.label, &project_map, &files);
    let corpus_stats = build_corpus_stats(&files);
    let graph = build_dependency_graph(&files);
    // Carry over prior embeddings on the silent-refresh path; bubble
    // errors when AI credentials are explicitly supplied. Per-file
    // pruning happens in carry_over_embeddings.
    let id_for_carryover = meta.id.clone();
    let embeddings = if let Some(ai) = ai {
        // Explicit user opt-in — surface failures so the UI can tell
        // the user "embedding refresh failed" rather than silently
        // serving stale vectors.
        Some(embed_files(ai, &files, prior_index.as_ref()).await?)
    } else {
        carry_over_embeddings(&id_for_carryover, &files).await
    };
    Ok(ProjectIndex {
        id: meta.id,
        label: meta.label,
        indexed_at: crate::brain::unix_secs(),
        root_hint: remote_root
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("project")
            .to_string(),
        files_indexed: files.len(),
        chunks_indexed,
        total_bytes,
        overview,
        project_digest,
        project_map,
        files,
        corpus_stats,
        graph,
        lineage: None,
        embeddings,
    })
}

/// RAM-resident store for REMOTE project indexes. Remote brains live on the
/// VPS (not the Mac's disk); they're hydrated into this cache when a VPS
/// connects and persisted back over SFTP. Keeping them here lets the existing
/// by-id reads (load_index/stats/is_expired/retrieve) work with no SSH session
/// in scope, exactly like the local-file path — the Mac just never writes them
/// to its own disk.
fn remote_cache() -> &'static tokio::sync::Mutex<HashMap<BrainId, Arc<ProjectIndex>>> {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<BrainId, Arc<ProjectIndex>>>> =
        OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Put a remote index into the RAM cache. Monotonic by `indexed_at`: a slower
/// build that finishes after a newer one can't demote the cache (closes the
/// reconnect-resync vs manual-Refresh clobber race).
pub async fn cache_put_remote(index: ProjectIndex) {
    let id = index.id.clone();
    let mut cache = remote_cache().lock().await;
    if let Some(existing) = cache.get(&id) {
        if existing.indexed_at > index.indexed_at {
            return; // a newer index already in cache — don't go backwards
        }
    }
    cache.insert(id, Arc::new(index));
}

/// Drop a remote index from the RAM cache (on disable).
pub async fn cache_remove_remote(id: &BrainId) {
    remote_cache().lock().await.remove(id);
}

/// Serialize a project index to JSON bytes (shared by local + remote writers).
pub fn encode_index(index: &ProjectIndex) -> AppResult<Vec<u8>> {
    serde_json::to_vec(index).map_err(|e| AppError::Internal(format!("encode project index: {e}")))
}

pub async fn write_index(index: &ProjectIndex) -> AppResult<()> {
    let path = storage::index_path(&index.id)?;
    let bytes = encode_index(index)?;
    storage::write_atomic(&path, &bytes).await
}

pub async fn load_index(id: &BrainId) -> AppResult<Option<ProjectIndex>> {
    // Remote brains live only in RAM on the Mac (their durable copy is on the
    // VPS). Check the cache first; fall through to the local file for local
    // brains.
    if let Some(idx) = remote_cache().lock().await.get(id) {
        return Ok(Some((**idx).clone()));
    }
    let path = storage::index_path(id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| AppError::Internal(format!("read project index: {e}")))?;
    let index = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("parse project index: {e}")))?;
    Ok(Some(index))
}

/// A remote brain's store lives INSIDE its own project folder: `<root>/.tersh`.
/// Context stays with the project — no central bloat, and deleting the project
/// folder takes its index with it. `root` must be an absolute remote path.
pub fn remote_store_dir(remote_root: &str) -> String {
    format!("{}/.tersh", remote_root.trim_end_matches('/'))
}

/// Pull the absolute project root out of a remote brain's scope.
fn remote_root_of(meta: &BrainMeta) -> AppResult<String> {
    match &meta.scope {
        crate::brain::BrainScope::Remote { remote_root, .. } => Ok(remote_root.clone()),
        crate::brain::BrainScope::Local { .. } => {
            Err(AppError::Internal("persist_remote on a local brain".into()))
        }
    }
}

/// Persist a freshly built REMOTE index to the VPS — `index.json` + `meta.json`
/// under `<project_root>/.tersh/` — and load it into the RAM cache. The Mac
/// never writes these to its own disk.
pub async fn persist_remote(
    session: &SshSession,
    meta: &BrainMeta,
    index: ProjectIndex,
) -> AppResult<()> {
    // Don't write a build that's already been superseded: if the RAM cache
    // holds a newer index for this brain (a concurrent refresh landed first),
    // skip the SFTP write so we never overwrite the VPS store with stale data.
    if let Some(existing) = load_index(&index.id).await.ok().flatten() {
        if existing.indexed_at > index.indexed_at {
            return Ok(());
        }
    }
    let dir = remote_store_dir(&remote_root_of(meta)?);
    let index_bytes = encode_index(&index)?;
    crate::sftp::write_remote_bytes(session, &format!("{dir}/index.json"), &index_bytes).await?;
    let meta_bytes = serde_json::to_vec_pretty(meta)
        .map_err(|e| AppError::Internal(format!("encode brain meta: {e}")))?;
    crate::sftp::write_remote_bytes(session, &format!("{dir}/meta.json"), &meta_bytes).await?;
    cache_put_remote(index).await;
    Ok(())
}

pub async fn stats(id: &BrainId) -> Option<IndexStats> {
    load_index(id).await.ok().flatten().map(|idx| {
        let mut languages = idx
            .files
            .iter()
            .map(|file| file.language.clone())
            .filter(|language| !language.is_empty() && language != "text")
            .collect::<Vec<_>>();
        languages.sort();
        languages.dedup();
        languages.truncate(8);

        let mut frameworks = idx.project_map.frameworks.clone();
        frameworks.sort();
        frameworks.dedup();
        frameworks.truncate(8);

        let mut capabilities = idx.project_map.capabilities.clone();
        capabilities.sort();
        capabilities.dedup();
        capabilities.truncate(8);

        let mut architecture = idx.project_map.architecture.clone();
        architecture.sort();
        architecture.dedup();
        architecture.truncate(6);

        let mut modules = idx.project_map.modules.clone();
        modules.sort();
        modules.dedup();
        modules.truncate(8);

        let (has_embeddings, embeddings_stale_since, embedding_model) = idx
            .embeddings
            .as_ref()
            .map(|e| (true, e.stale_since, e.model.clone()))
            .unwrap_or((false, None, String::new()));

        IndexStats {
            indexed_at: idx.indexed_at,
            files_indexed: idx.files_indexed,
            chunks_indexed: idx.chunks_indexed,
            overview: compact_overview(&idx.overview),
            project_digest: compact_overview(&idx.project_digest),
            languages,
            frameworks,
            capabilities,
            architecture,
            modules,
            has_embeddings,
            embeddings_stale_since,
            embedding_model,
        }
    })
}

fn compact_overview(overview: &str) -> String {
    const MAX_OVERVIEW_CHARS: usize = 260;
    let single_line = overview
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if single_line.chars().count() <= MAX_OVERVIEW_CHARS {
        single_line
    } else {
        let mut out = single_line
            .chars()
            .take(MAX_OVERVIEW_CHARS.saturating_sub(1))
            .collect::<String>();
        out.push('…');
        out
    }
}

pub async fn is_expired(id: &BrainId, max_age_secs: i64) -> AppResult<bool> {
    let Some(index) = load_index(id).await? else {
        return Ok(true);
    };
    let age = crate::brain::unix_secs().saturating_sub(index.indexed_at);
    Ok(age >= max_age_secs)
}

pub async fn retrieve_context_with_trace(
    id: &BrainId,
    prompt: &str,
    ai: Option<&EmbeddingConfig>,
) -> AppResult<Option<RetrievedContext>> {
    // No keyword self-gate here. The caller (prompt_enhance via `brain_enabled`)
    // is the single source of truth for whether to use the index — gating again
    // on `should_use_project_context` would re-introduce the brittle keyword miss
    // (e.g. "not work well") for Planning/Research/Question/Unclear intents and
    // silently inject no context even though the index is in use.
    let Some(index) = load_index(id).await? else {
        return Ok(None);
    };
    let query_vector = if let (Some(ai), Some(_)) = (ai, index.embeddings.as_ref()) {
        match embed::embed_batch(ai, std::slice::from_ref(&prompt.to_string())).await {
            Ok(mut v) => v.pop(),
            Err(err) => {
                tracing::warn!("query embed failed: {err}; falling back to TF-IDF+n-gram only");
                None
            }
        }
    } else {
        None
    };
    Ok(Some(build_context(&index, prompt, query_vector.as_deref())))
}

// Only the classifier unit tests use this now — the production gate is
// `brain_enabled` in prompt_enhance (retrieval-by-default, not keyword-gated).
#[cfg(test)]
pub fn should_use_project_context(prompt: &str) -> bool {
    decide_project_context(prompt).use_project_context
}

/// Generalised greenfield detector: a "build/create/make … a <artifact>"
/// request with no reference to existing code. Catches phrasing the literal
/// list misses — e.g. "build me a free screenshot API service", "write a
/// Discord bot", "create a CLI" — without firing on existing-code edits like
/// "make the api faster" (definite article = existing thing, no match).
fn looks_like_greenfield_build(p: &str) -> bool {
    let create_phrase = contains_any(
        p,
        &[
            "build a ",
            "build an ",
            "build me a ",
            "build me an ",
            "build a new ",
            "create a ",
            "create an ",
            "create me a ",
            "create a new ",
            "make a ",
            "make an ",
            "make me a ",
            "make me an ",
            "write a ",
            "write an ",
            "write me a ",
            "scaffold a ",
            "scaffold an ",
            "generate a ",
            "generate an ",
            "develop a ",
            "develop an ",
            "set up a ",
            "set up an ",
            "spin up a ",
            "bootstrap a ",
            "implement a ",
            "implement an ",
            "i need a ",
            "i want a ",
            "i want to build a ",
            "help me build a ",
        ],
    );
    let artifact = contains_any(
        p,
        &[
            "api",
            "service",
            "microservice",
            "server",
            "backend",
            "frontend",
            "app",
            "application",
            "website",
            "web app",
            "webapp",
            "site",
            "landing page",
            "dashboard",
            "tool",
            "cli",
            "command line",
            "bot",
            "script",
            "library",
            "package",
            "module",
            "sdk",
            "wrapper",
            "plugin",
            "extension",
            "game",
            "scraper",
            "crawler",
            "pipeline",
            "saas",
            "platform",
            "endpoint",
            "webhook",
            "proxy",
            "boilerplate",
            "starter",
            "mvp",
            "clone of",
        ],
    );
    create_phrase && artifact
}

pub fn decide_project_context(prompt: &str) -> PromptContextDecision {
    let p = prompt.to_lowercase();
    let fresh = looks_like_greenfield_build(&p)
        || contains_any(
            &p,
            &[
                "fresh app",
                "fresh project",
                "brand new app",
                "brand new project",
                "new app",
                "new project",
                "new website",
                "new dashboard",
                "new tool",
                "new product",
                "start a project",
                "start an app",
                "start a website",
                "from scratch",
                "scratch app",
                "scratch project",
                "build an app",
                "build a website",
                "build me an app",
                "build me a website",
                "build a dashboard",
                "build a tool",
                "make an app",
                "make a website",
                "make me an app",
                "make me a website",
                "make a dashboard",
                "create an app",
                "create a website",
                "create a dashboard",
                "create a tool",
                "food delivery app",
                "marketplace app",
                "crm",
                "admin panel",
                "customer account",
                "vendor account",
                "saas",
                "landing page",
            ],
        );
    let repo_specific = prompt_has_repo_marker(&p);
    let bug = contains_any(
        &p,
        &[
            "bug",
            "fix",
            "broken",
            "doesn't work",
            "doesnt work",
            "not working",
            "error",
            "crash",
            "panic",
            "regression",
            "stack trace",
            "traceback",
            "typeerror",
            "failed",
            "slow",
            "lag",
            "freeze",
            "frozen",
        ],
    );
    let planning = contains_any(
        &p,
        &[
            "plan",
            "planning",
            "architecture",
            "design",
            "what do you think",
            "advise",
            "approach",
            "proposal",
            "roadmap",
            "strategy",
        ],
    );
    let research = contains_any(
        &p,
        &["research", "compare", "investigate", "look up", "reference"],
    );
    let question = p.trim_end().ends_with('?')
        || contains_any(&p, &["what is", "why", "how does", "explain", "what does"]);

    if fresh && !repo_specific && !bug {
        return PromptContextDecision {
            kind: PromptIntentKind::FreshBuild,
            use_project_context: false,
            reason: "Fresh-build prompt; repo context would add noise.".into(),
        };
    }

    if bug {
        return PromptContextDecision {
            kind: PromptIntentKind::BugFix,
            use_project_context: true,
            reason: "Bug-fix prompt; existing implementation context is likely needed.".into(),
        };
    }

    if repo_specific {
        return PromptContextDecision {
            kind: PromptIntentKind::RepoChange,
            use_project_context: true,
            reason: "Repo-specific change; use indexed project context to avoid guessing.".into(),
        };
    }

    if research {
        return PromptContextDecision {
            kind: PromptIntentKind::Research,
            use_project_context: false,
            reason: "Research prompt without a clear project target; skip repo reads.".into(),
        };
    }

    if planning {
        return PromptContextDecision {
            kind: PromptIntentKind::Planning,
            use_project_context: false,
            reason: "Planning prompt without an existing-code marker; keep it general.".into(),
        };
    }

    if question {
        return PromptContextDecision {
            kind: PromptIntentKind::Question,
            use_project_context: false,
            reason: "General question; no project context needed unless the prompt names the repo."
                .into(),
        };
    }

    PromptContextDecision {
        kind: PromptIntentKind::Unclear,
        use_project_context: false,
        reason: "No strong signal that project files are needed.".into(),
    }
}

fn build_local_blocking(
    meta: BrainMeta,
    root: PathBuf,
    prior_index: Option<&ProjectIndex>,
) -> AppResult<ProjectIndex> {
    let ignores = IgnoreRules::load(&root);
    let mut candidates = Vec::new();
    collect_files(&root, &root, &ignores, &mut candidates)?;
    let prior_files = prior_file_map(prior_index);

    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    for path in candidates {
        if files.len() >= MAX_INDEX_FILES || total_bytes >= MAX_TOTAL_BYTES {
            break;
        }
        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let abs_str = path.to_string_lossy().into_owned();
        if secrets::path_skip_reason(&abs_str).is_some() || !looks_like_source_or_doc(&path) {
            continue;
        }
        let Ok(meta_fs) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta_fs.is_file() || meta_fs.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if secrets::content_secret_kind(&bytes).is_some() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if text.contains('\0') {
            continue;
        }
        let content_hash = content_hash(&bytes);
        total_bytes = total_bytes.saturating_add(meta_fs.len());
        files.push(index_text_file(
            &rel,
            meta_fs.len(),
            modified_secs(&meta_fs),
            text,
            content_hash,
            prior_files.get(rel.as_str()).copied(),
        ));
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let chunks_indexed = files.iter().map(|f| f.chunks.len()).sum();
    let project_map = build_project_map(&files);
    let overview = build_overview(&meta.label, &project_map, &files);
    let project_digest = build_project_digest(&meta.label, &project_map, &files);
    let corpus_stats = build_corpus_stats(&files);
    let graph = build_dependency_graph(&files);
    Ok(ProjectIndex {
        id: meta.id,
        label: meta.label,
        indexed_at: crate::brain::unix_secs(),
        root_hint: root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string(),
        files_indexed: files.len(),
        chunks_indexed,
        total_bytes,
        overview,
        project_digest,
        project_map,
        files,
        corpus_stats,
        graph,
        // Lineage stitched in by the async wrapper `build_local`.
        lineage: None,
        // Embeddings stitched in by the async wrapper when AI settings
        // are supplied.
        embeddings: None,
    })
}

fn prior_file_map(index: Option<&ProjectIndex>) -> HashMap<&str, &IndexedFile> {
    index
        .map(|idx| {
            idx.files
                .iter()
                .map(|file| (file.path.as_str(), file))
                .collect()
        })
        .unwrap_or_default()
}

fn index_text_file(
    rel: &str,
    size: u64,
    modified: i64,
    text: &str,
    content_hash: String,
    prior: Option<&IndexedFile>,
) -> IndexedFile {
    if let Some(prior) = prior {
        let hash_matches = !prior.content_hash.is_empty() && prior.content_hash == content_hash;
        let legacy_matches =
            prior.content_hash.is_empty() && prior.size == size && prior.modified == modified;
        if hash_matches || legacy_matches {
            let mut reused = prior.clone();
            reused.size = size;
            reused.modified = modified;
            reused.content_hash = content_hash;
            return reused;
        }
    }

    let language = language_for_path(Path::new(rel)).to_string();
    let role = infer_role(rel, &language, text);
    let imports = extract_imports(text, &language);
    let symbols = extract_symbols(text, &language);
    let summary = summarize_file(rel, &language, &role, text, &symbols, &imports);
    let chunks = chunk_text(text);
    IndexedFile {
        path: rel.to_string(),
        language,
        size,
        modified,
        content_hash,
        role,
        imports,
        symbols,
        summary,
        chunks,
    }
}

fn content_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[derive(Debug)]
struct RemoteCandidate {
    rel: String,
    path: String,
    size: u64,
    modified: i64,
}

async fn collect_remote_files(
    session: &Arc<SshSession>,
    remote_root: &str,
) -> AppResult<Vec<RemoteCandidate>> {
    let root = remote_root.trim_end_matches('/');
    if root.is_empty() || !root.starts_with('/') {
        return Err(AppError::Invalid(
            "remote project root must be absolute".into(),
        ));
    }
    let cmd = format!(
        "cd {root_q} 2>/dev/null && find . \
            \\( -name node_modules -o -name target -o -name dist -o -name build -o -name .next -o -name .turbo -o -name .git -o -name vendor -o -name coverage -o -name __pycache__ -o -name .venv -o -name .cache \\) -prune -o \
            -type f -size -{max_file}c -printf '%s\\t%T@\\t%P\\n' 2>/dev/null | head -n {limit}",
        root_q = shell_quote(root),
        max_file = MAX_FILE_BYTES + 1,
        limit = MAX_INDEX_FILES
    );
    let out = session.exec_oneshot(&cmd, 512 * 1024).await?;
    let text = String::from_utf8_lossy(&out);
    let mut candidates = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(size_s) = parts.next() else {
            continue;
        };
        let Some(modified_s) = parts.next() else {
            continue;
        };
        let Some(rel) = parts.next() else {
            continue;
        };
        if rel.is_empty()
            || rel.starts_with('/')
            || rel.contains("/../")
            || rel == ".."
            || rel.ends_with("/..")
        {
            continue;
        }
        if rel.split('/').any(should_skip_name) {
            continue;
        }
        let size = size_s.parse::<u64>().unwrap_or(0);
        let modified = modified_s
            .split('.')
            .next()
            .unwrap_or("0")
            .parse::<i64>()
            .unwrap_or(0);
        let path = format!("{root}/{rel}");
        candidates.push(RemoteCandidate {
            rel: rel.to_string(),
            path,
            size,
            modified,
        });
    }
    candidates.sort_by(|a, b| {
        remote_candidate_priority(&a.rel)
            .cmp(&remote_candidate_priority(&b.rel))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    Ok(candidates)
}

fn remote_candidate_priority(path: &str) -> u8 {
    if is_manifest(path) {
        0
    } else if is_config_file(path) || looks_like_entrypoint(path, "") {
        1
    } else if is_test_file(path) {
        3
    } else if path.ends_with(".md") || path.ends_with(".mdx") {
        4
    } else {
        2
    }
}

fn collect_files(
    root: &Path,
    dir: &Path,
    ignores: &IgnoreRules,
    out: &mut Vec<PathBuf>,
) -> AppResult<()> {
    if out.len() >= MAX_INDEX_FILES {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::Invalid(format!("read project directory: {e}")))?
        .flatten()
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_name(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if let Ok(rel) = path.strip_prefix(root) {
            if ignores.is_ignored(rel, file_type.is_dir()) {
                continue;
            }
        }
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel_s = rel.to_string_lossy();
                if secrets::path_skip_reason(&format!("/{rel_s}/")).is_some() {
                    continue;
                }
            }
            collect_files(root, &path, ignores, out)?;
        } else if file_type.is_file() {
            out.push(path);
            if out.len() >= MAX_INDEX_FILES {
                break;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct IgnoreRules {
    patterns: Vec<IgnorePattern>,
}

#[derive(Debug)]
struct IgnorePattern {
    raw: String,
    directory_only: bool,
    rooted: bool,
}

impl IgnoreRules {
    fn load(root: &Path) -> Self {
        let mut rules = Self::default();
        for file in [".gitignore", ".tershignore", ".augmentignore"] {
            let path = root.join(file);
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                    continue;
                }
                let directory_only = line.ends_with('/');
                let rooted = line.starts_with('/');
                let raw = line
                    .trim_start_matches('/')
                    .trim_end_matches('/')
                    .trim()
                    .to_string();
                if raw.is_empty() {
                    continue;
                }
                rules.patterns.push(IgnorePattern {
                    raw,
                    directory_only,
                    rooted,
                });
            }
        }
        rules
    }

    fn is_ignored(&self, rel: &Path, is_dir: bool) -> bool {
        let rel = rel.to_string_lossy().replace('\\', "/");
        let file_name = rel.rsplit('/').next().unwrap_or(&rel);
        self.patterns.iter().any(|p| {
            let pat = p.raw.as_str();
            if p.directory_only && !is_dir && !rel.contains(&format!("{pat}/")) {
                return false;
            }
            if let Some(suffix) = pat.strip_prefix("*.") {
                return file_name.ends_with(&format!(".{suffix}"));
            }
            if p.rooted {
                return rel == pat || rel.starts_with(&format!("{pat}/"));
            }
            if pat.contains('/') {
                return rel == pat
                    || rel.ends_with(&format!("/{pat}"))
                    || rel.contains(&format!("/{pat}/"));
            }
            file_name == pat || rel.contains(&format!("/{pat}/"))
        })
    }
}

fn should_skip_name(name: &str) -> bool {
    if name.starts_with('.')
        && !matches!(
            name,
            ".github" | ".gitignore" | ".augmentignore" | ".tershignore"
        )
    {
        return true;
    }
    matches!(
        name,
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
    )
}

fn looks_like_source_or_doc(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if matches!(
        name,
        "README" | "README.md" | "package.json" | "Cargo.toml" | "pyproject.toml" | "go.mod"
    ) {
        return true;
    }
    matches!(
        path.extension().and_then(|s| s.to_str()).unwrap_or(""),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "json"
            | "toml"
            | "md"
            | "mdx"
            | "py"
            | "go"
            | "swift"
            | "java"
            | "kt"
            | "kts"
            | "php"
            | "rb"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "css"
            | "scss"
            | "html"
            | "svelte"
            | "vue"
            | "yml"
            | "yaml"
    )
}

fn language_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "swift" => "swift",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "php" => "php",
        "rb" => "ruby",
        "css" | "scss" => "css",
        "html" => "html",
        "md" | "mdx" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        _ => "text",
    }
}

fn infer_role(path: &str, language: &str, text: &str) -> String {
    let lower = path.to_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    let role = if is_manifest(path) {
        "manifest"
    } else if is_config_file(path) {
        "config"
    } else if is_test_file(path) {
        "test"
    } else if matches!(
        file_name,
        "readme.md" | "readme" | "changelog.md" | "license" | "security.md"
    ) || language == "markdown"
    {
        "documentation"
    } else if looks_like_entrypoint(path, text) {
        "entrypoint"
    } else if lower.contains("/components/") || lower.contains("/ui/") {
        "ui component"
    } else if lower.contains("/api/") || lower.contains("/routes/") || lower.contains("/pages/api/")
    {
        "api route"
    } else if lower.contains("/store/") || lower.contains("/state/") || lower.contains("reducer") {
        "state"
    } else if lower.contains("/test") || lower.contains("/__tests__/") {
        "test"
    } else {
        "source"
    };
    role.to_string()
}

fn extract_imports(text: &str, language: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().take(1_200) {
        let t = line.trim();
        let import = match language {
            "typescript" | "javascript" => extract_js_import(t),
            "rust" => t.strip_prefix("use ").map(|s| {
                s.trim_end_matches(';')
                    .split("::")
                    .next()
                    .unwrap_or(s)
                    .to_string()
            }),
            "python" => t
                .strip_prefix("import ")
                .or_else(|| t.strip_prefix("from "))
                .map(|s| {
                    s.split_whitespace()
                        .next()
                        .unwrap_or(s)
                        .trim_end_matches(',')
                        .to_string()
                }),
            "go" => {
                if t.starts_with('"') && t.ends_with('"') {
                    Some(t.trim_matches('"').to_string())
                } else {
                    t.strip_prefix("import ")
                        .map(|s| s.trim().trim_matches('"').to_string())
                }
            }
            "swift" => t.strip_prefix("import ").map(|s| s.trim().to_string()),
            "java" | "kotlin" => t
                .strip_prefix("import ")
                .map(|s| s.trim_end_matches(';').to_string()),
            _ => None,
        };
        if let Some(import) = import {
            let import = import
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if import.len() >= 2 && import.len() <= 120 {
                out.push(import);
            }
        }
        if out.len() >= 64 {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn extract_js_import(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("import ") {
        if let Some(idx) = rest.find(" from ") {
            return quoted_module(&rest[idx + 6..]);
        }
        return quoted_module(rest);
    }
    if let Some(idx) = line.find("require(") {
        return quoted_module(&line[idx + "require(".len()..]);
    }
    if let Some(idx) = line.find("import(") {
        return quoted_module(&line[idx + "import(".len()..]);
    }
    None
}

fn quoted_module(text: &str) -> Option<String> {
    let s = text.trim();
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' && quote != '`' {
        return None;
    }
    let rest = &s[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn extract_symbols(text: &str, language: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().take(2_000) {
        let t = line.trim();
        let hit = match language {
            "rust" => t
                .strip_prefix("pub fn ")
                .or_else(|| t.strip_prefix("fn "))
                .or_else(|| t.strip_prefix("pub struct "))
                .or_else(|| t.strip_prefix("struct "))
                .or_else(|| t.strip_prefix("pub enum "))
                .or_else(|| t.strip_prefix("enum ")),
            "typescript" | "javascript" => t
                .strip_prefix("export function ")
                .or_else(|| t.strip_prefix("function "))
                .or_else(|| t.strip_prefix("export const "))
                .or_else(|| t.strip_prefix("const "))
                .or_else(|| t.strip_prefix("export class "))
                .or_else(|| t.strip_prefix("class ")),
            "python" => t.strip_prefix("def ").or_else(|| t.strip_prefix("class ")),
            "go" => t.strip_prefix("func ").or_else(|| t.strip_prefix("type ")),
            "markdown" => t.strip_prefix("# "),
            _ => None,
        };
        if let Some(rest) = hit {
            let name = rest
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '#'))
                .next()
                .unwrap_or("")
                .trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
        if out.len() >= 24 {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn summarize_file(
    path: &str,
    language: &str,
    role: &str,
    text: &str,
    symbols: &[String],
    imports: &[String],
) -> String {
    let first_meaningful = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
        .unwrap_or("");
    let symbols = if symbols.is_empty() {
        "no prominent symbols".to_string()
    } else {
        symbols
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let imports = if imports.is_empty() {
        "no imports".to_string()
    } else {
        imports
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let first = first_meaningful.chars().take(180).collect::<String>();
    format!("{path} ({language}, {role}) — symbols: {symbols}. Imports: {imports}. First signal: {first}")
}

fn chunk_text(text: &str) -> Vec<IndexChunk> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < lines.len() && chunks.len() < 32 {
        let end = (start + CHUNK_LINES).min(lines.len());
        let mut chunk = lines[start..end].join("\n");
        if chunk.chars().count() > MAX_CHUNK_CHARS {
            chunk = chunk.chars().take(MAX_CHUNK_CHARS).collect();
        }
        chunks.push(IndexChunk {
            start_line: start + 1,
            end_line: end,
            keywords: top_keywords(&chunk, 16),
            text: chunk,
        });
        if end == lines.len() {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP);
    }
    chunks
}

fn build_project_map(files: &[IndexedFile]) -> ProjectMap {
    let mut map = ProjectMap::default();
    let mut deps = HashSet::new();
    let mut scripts = HashSet::new();
    let mut modules: HashMap<String, ModuleStats> = HashMap::new();
    for file in files {
        collect_module_stats(&mut modules, file);
        if is_manifest(&file.path) {
            map.manifests.push(file.path.clone());
            detect_package_manager(&file.path, &mut map.package_managers);
            extract_manifest_dependencies(file, &mut deps);
            extract_project_scripts(file, &mut scripts);
        }
        if file.role == "entrypoint" {
            map.entrypoints.push(file.path.clone());
        }
        if file.role == "config" {
            map.config_files.push(file.path.clone());
        }
        if file.role == "test" {
            map.test_files.push(file.path.clone());
        }
        if file.role == "documentation" {
            map.doc_files.push(file.path.clone());
        }
        detect_frameworks(file, &mut map.frameworks);
        detect_capabilities(file, &mut map.capabilities);
    }
    map.package_managers.sort();
    map.package_managers.dedup();
    map.frameworks.sort();
    map.frameworks.dedup();
    map.capabilities.sort();
    map.capabilities.dedup();
    map.architecture = infer_architecture(files, &map);
    map.manifests.sort();
    map.entrypoints.sort();
    map.config_files.sort();
    map.test_files.sort();
    map.doc_files.sort();
    let mut module_rows = modules
        .into_iter()
        .map(|(name, stats)| {
            (
                module_score(&name, &stats),
                format!(
                    "{} ({} files; roles: {})",
                    name,
                    stats.files,
                    compact_role_counts(&stats.roles)
                ),
            )
        })
        .collect::<Vec<_>>();
    module_rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    map.modules = module_rows
        .into_iter()
        .take(18)
        .map(|(_, row)| row)
        .collect();
    let mut deps = deps.into_iter().collect::<Vec<_>>();
    deps.sort();
    deps.truncate(80);
    map.dependencies = deps;
    let mut scripts = scripts.into_iter().collect::<Vec<_>>();
    scripts.sort();
    scripts.truncate(30);
    map.scripts = scripts;
    map
}

#[derive(Debug, Clone, Default)]
struct ModuleStats {
    files: usize,
    roles: HashMap<String, usize>,
}

fn collect_module_stats(modules: &mut HashMap<String, ModuleStats>, file: &IndexedFile) {
    let Some(module) = module_bucket_for_path(&file.path) else {
        return;
    };
    let stats = modules.entry(module).or_default();
    stats.files = stats.files.saturating_add(1);
    *stats.roles.entry(file.role.clone()).or_default() += 1;
}

fn module_bucket_for_path(path: &str) -> Option<String> {
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }

    match parts[0] {
        "src" | "app" | "frontend" | "backend" | "crates" | "packages" => {
            Some(format!("{}/{}", parts[0], parts[1]))
        }
        first => Some(first.to_string()),
    }
}

fn module_score(name: &str, stats: &ModuleStats) -> usize {
    let mut score = stats.files;
    for (role, count) in &stats.roles {
        let weight = match role.as_str() {
            "entrypoint" => 8,
            "manifest" => 7,
            "config" => 5,
            "source" => 3,
            "test" => 2,
            "documentation" => 1,
            _ => 1,
        };
        score = score.saturating_add(count.saturating_mul(weight));
    }
    if matches!(
        name,
        "src" | "app" | "frontend/src" | "backend/src" | "src/components" | "src/app"
    ) {
        score = score.saturating_add(8);
    }
    score
}

fn compact_role_counts(roles: &HashMap<String, usize>) -> String {
    if roles.is_empty() {
        return "unknown".to_string();
    }
    let mut rows = roles
        .iter()
        .map(|(role, count)| (role.as_str(), *count))
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.into_iter()
        .take(4)
        .map(|(role, count)| format!("{role}:{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn build_overview(label: &str, project_map: &ProjectMap, files: &[IndexedFile]) -> String {
    let mut langs: HashMap<&str, usize> = HashMap::new();
    let mut top = Vec::new();
    for file in files {
        *langs.entry(file.language.as_str()).or_default() += 1;
        if top.len() < 18
            && matches!(
                file.role.as_str(),
                "entrypoint" | "manifest" | "config" | "source"
            )
        {
            top.push(file.summary.clone());
        }
    }
    let mut lang_bits = langs
        .into_iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>();
    lang_bits.sort();
    format!(
        "Project {label}. Indexed {} files. Languages: {}.\nPackage managers: {}.\nFramework hints: {}.\nCapabilities: {}.\nMain modules: {}.\nEntrypoints: {}.\nUseful scripts: {}.\nKey files:\n{}",
        files.len(),
        lang_bits.join(", "),
        list_or_none(&project_map.package_managers),
        list_or_none(&project_map.frameworks),
        list_or_none(&project_map.capabilities),
        list_or_none(&project_map.modules),
        list_or_none(&project_map.entrypoints.iter().take(10).cloned().collect::<Vec<_>>()),
        list_or_none(&project_map.scripts.iter().take(10).cloned().collect::<Vec<_>>()),
        top.iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn build_project_digest(label: &str, project_map: &ProjectMap, files: &[IndexedFile]) -> String {
    let mut source_roots = files
        .iter()
        .filter(|file| matches!(file.role.as_str(), "source" | "entrypoint"))
        .filter_map(|file| module_bucket_for_path(&file.path))
        .collect::<Vec<_>>();
    source_roots.sort();
    source_roots.dedup();
    source_roots.truncate(8);

    let verification = project_map
        .scripts
        .iter()
        .filter(|script| {
            let lower = script.to_lowercase();
            lower.contains("test")
                || lower.contains("type")
                || lower.contains("lint")
                || lower.contains("check")
                || lower.contains("build")
        })
        .take(8)
        .cloned()
        .collect::<Vec<_>>();

    let mut critical_files = Vec::new();
    critical_files.extend(project_map.manifests.iter().take(6).cloned());
    critical_files.extend(project_map.entrypoints.iter().take(8).cloned());
    critical_files.extend(project_map.config_files.iter().take(8).cloned());
    critical_files.sort();
    critical_files.dedup();
    critical_files.truncate(16);

    format!(
        "Project digest for {label}:\n\
        - Shape: {} indexed files across source roots {}.\n\
        - Stack: package managers {}; frameworks {}; notable dependencies {}.\n\
        - Capabilities: {}.\n\
        - Architecture signals: {}.\n\
        - Main modules: {}.\n\
        - Critical files: {}.\n\
        - Verification commands: {}.\n\
        - Agent rule: treat this digest as orientation, then use matched files or tools for exact implementation details.",
        files.len(),
        list_or_none(&source_roots),
        list_or_none(&project_map.package_managers),
        list_or_none(&project_map.frameworks),
        list_or_none(
            &project_map
                .dependencies
                .iter()
                .take(18)
                .cloned()
                .collect::<Vec<_>>()
        ),
        list_or_none(&project_map.capabilities),
        list_or_none(&project_map.architecture),
        list_or_none(
            &project_map
                .modules
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
        ),
        list_or_none(&critical_files),
        list_or_none(&verification),
    )
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none detected".to_string()
    } else {
        items
            .iter()
            .take(18)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, Copy)]
enum PromptIntent {
    BugFix,
    Feature,
    Refactor,
    Test,
    Ui,
    Security,
    Research,
    General,
}

impl PromptIntent {
    fn as_str(self) -> &'static str {
        match self {
            PromptIntent::BugFix => "bug-fix / regression",
            PromptIntent::Feature => "feature implementation",
            PromptIntent::Refactor => "refactor / cleanup",
            PromptIntent::Test => "test / verification",
            PromptIntent::Ui => "UI / interaction",
            PromptIntent::Security => "security / hardening",
            PromptIntent::Research => "planning / research",
            PromptIntent::General => "general coding task",
        }
    }
}

fn build_context(
    index: &ProjectIndex,
    prompt: &str,
    query_vector: Option<&[f32]>,
) -> RetrievedContext {
    let intent = infer_prompt_intent(prompt);
    let terms = query_terms(prompt);
    let query_ngrams = ngram::ngrams(prompt);
    let prompt_l = prompt.to_lowercase();
    let now = crate::brain::unix_secs();
    let lineage = index.lineage.as_ref();
    let embeddings = index.embeddings.as_ref();
    let mut file_scores = Vec::new();
    let mut chunk_scores = Vec::new();
    for file in &index.files {
        let file_text = format!("{} {} {}", file.path, file.summary, file.symbols.join(" "));
        let semantic = score_file_v2(
            file,
            &terms,
            &query_ngrams,
            &index.corpus_stats,
            lineage,
            intent,
            now,
            embeddings,
            query_vector,
        );
        let score = semantic.saturating_add(path_mention_bonus(file, &prompt_l));
        if score > 0 {
            file_scores.push((score, file));
        }
        for chunk in &file.chunks {
            let chunk_text_score = score_text(
                &format!("{} {} {}", file_text, chunk.keywords.join(" "), chunk.text),
                &terms,
            );
            let chunk_total = score.saturating_add(chunk_text_score);
            if chunk_total > 0 {
                chunk_scores.push((chunk_total, file, chunk));
            }
        }
    }
    file_scores.sort_by(|a, b| b.0.cmp(&a.0));
    chunk_scores.sort_by(|a, b| b.0.cmp(&a.0));

    let selected_files = file_scores
        .iter()
        .take(10)
        .map(|(_, file)| *file)
        .collect::<Vec<_>>();
    let related_files = related_file_summaries(index, &selected_files);
    let mut trace = vec![RetrievedContextTraceItem {
        tool: "index_context".to_string(),
        target: Some(index.root_hint.clone()),
        status: format!(
            "selected indexed context for {}; {} files indexed",
            intent.as_str(),
            index.files_indexed
        ),
    }];
    trace.extend(
        selected_files
            .iter()
            .take(8)
            .map(|file| RetrievedContextTraceItem {
                tool: "index_file".to_string(),
                target: Some(file.path.clone()),
                status: format!("selected {} file", file.role),
            }),
    );

    let mut out = String::new();
    out.push_str("Indexed project context:\n");
    out.push_str(&index.overview);
    if !index.project_digest.trim().is_empty() {
        out.push_str("\n\nStored project digest:\n");
        out.push_str(index.project_digest.trim());
    }
    out.push_str("\n\nContext strategy:\n");
    let file_strategy = if selected_files.is_empty() {
        "No file was confidently matched by the index. Use the project map for orientation, then use tools only for targeted confirmation if the prompt names a concrete file, component, command, or feature."
    } else {
        "Prefer the files below first. Use the attached tools only for exact line-level confirmation or missing details. Related files are included because they import, are imported by, or sit next to the top matches."
    };
    out.push_str(&format!(
        "- Detected request type: {}\n- Guidance: {}\n- File strategy: {}\n",
        intent.as_str(),
        intent_guidance(intent),
        file_strategy
    ));
    out.push_str("\n\nProject map:\n");
    out.push_str(&format!(
        "- Capabilities: {}\n- Architecture: {}\n- Main modules: {}\n- Manifests: {}\n- Config: {}\n- Tests: {}\n- Docs: {}\n- Scripts: {}\n- Dependencies: {}\n",
        list_or_none(&index.project_map.capabilities),
        list_or_none(&index.project_map.architecture),
        list_or_none(&index.project_map.modules),
        list_or_none(&index.project_map.manifests),
        list_or_none(
            &index
                .project_map
                .config_files
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
        ),
        list_or_none(
            &index
                .project_map
                .test_files
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
        ),
        list_or_none(
            &index
                .project_map
                .doc_files
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
        ),
        list_or_none(
            &index
                .project_map
                .scripts
                .iter()
                .take(16)
                .cloned()
                .collect::<Vec<_>>()
        ),
        list_or_none(
            &index
                .project_map
                .dependencies
                .iter()
                .take(30)
                .cloned()
                .collect::<Vec<_>>()
        ),
    ));
    if !selected_files.is_empty() {
        out.push_str("\n\nMost relevant files:\n");
        for file in selected_files {
            out.push_str("- ");
            out.push_str(&file.summary);
            out.push('\n');
        }
    }
    if !related_files.is_empty() {
        out.push_str("\nRelated files worth checking:\n");
        for summary in related_files {
            out.push_str("- ");
            out.push_str(&summary);
            out.push('\n');
        }
    }
    let mut seen = HashSet::new();
    let mut chunk_trace_count = 0_usize;
    let mut wrote_chunk_heading = false;
    for (_, file, chunk) in chunk_scores {
        let key = format!("{}:{}-{}", file.path, chunk.start_line, chunk.end_line);
        if !seen.insert(key.clone()) {
            continue;
        }
        if !wrote_chunk_heading {
            out.push_str("\nRelevant indexed chunks:\n");
            wrote_chunk_heading = true;
        }
        if chunk_trace_count < 8 {
            trace.push(RetrievedContextTraceItem {
                tool: "index_chunk".to_string(),
                target: Some(key.clone()),
                status: "selected matching chunk".to_string(),
            });
            chunk_trace_count += 1;
        }
        out.push_str(&format!("\n--- {key} ---\n"));
        out.push_str(&chunk.text);
        out.push('\n');
        if out.chars().count() > MAX_CONTEXT_CHARS {
            break;
        }
    }
    RetrievedContext {
        text: out.chars().take(MAX_CONTEXT_CHARS).collect(),
        trace,
    }
}

fn infer_prompt_intent(prompt: &str) -> PromptIntent {
    let p = prompt.to_lowercase();
    if contains_any(
        &p,
        &[
            "security",
            "vuln",
            "vulnerability",
            "exploit",
            "xss",
            "csrf",
            "secret",
            "threat",
            "harden",
        ],
    ) {
        PromptIntent::Security
    } else if contains_any(
        &p,
        &[
            "bug",
            "fix",
            "broken",
            "regression",
            "doesn't work",
            "doesnt work",
            "error",
            "crash",
            "freeze",
            "slow",
        ],
    ) {
        PromptIntent::BugFix
    } else if contains_any(&p, &["test", "spec", "coverage", "verify", "assert"]) {
        PromptIntent::Test
    } else if contains_any(
        &p,
        &[
            "ui", "ux", "design", "style", "button", "drawer", "sidebar", "screen", "layout",
            "polish",
        ],
    ) {
        PromptIntent::Ui
    } else if contains_any(
        &p,
        &["refactor", "cleanup", "simplify", "rename", "restructure"],
    ) {
        PromptIntent::Refactor
    } else if contains_any(
        &p,
        &["research", "investigate", "compare", "explain", "plan"],
    ) {
        PromptIntent::Research
    } else if contains_any(
        &p,
        &["add", "implement", "build", "create", "support", "allow"],
    ) {
        PromptIntent::Feature
    } else {
        PromptIntent::General
    }
}

fn intent_guidance(intent: PromptIntent) -> &'static str {
    match intent {
        PromptIntent::BugFix => {
            "identify the failing behavior, likely owning modules, call paths, lifecycle/state risks, and verification steps before proposing edits"
        }
        PromptIntent::Feature => {
            "connect the requested feature to existing entrypoints, state owners, commands, and UI surfaces; preserve local architecture instead of inventing a parallel system"
        }
        PromptIntent::Refactor => {
            "describe the existing boundaries first, then keep the rewrite scoped to the smallest module set that removes real complexity"
        }
        PromptIntent::Test => {
            "find the nearest existing test style and suggest focused coverage for the files and behaviors most likely to regress"
        }
        PromptIntent::Ui => {
            "ground the UI change in the current components, styles, theme tokens, and interaction states rather than generic design advice"
        }
        PromptIntent::Security => {
            "map trust boundaries and sensitive data paths, then separate confirmed vulnerabilities from hardening ideas"
        }
        PromptIntent::Research => {
            "summarize the current implementation shape, known constraints, and the exact unknowns to inspect next"
        }
        PromptIntent::General => {
            "use the project map to infer the relevant layer, then ask for missing details only if the request cannot be safely scoped"
        }
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn path_mention_bonus(file: &IndexedFile, prompt_l: &str) -> usize {
    let path_l = file.path.to_lowercase();
    let basename_l = file
        .path
        .rsplit('/')
        .next()
        .unwrap_or(file.path.as_str())
        .to_lowercase();
    let stem_l = basename_l
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| basename_l.clone());

    let mut bonus = 0_usize;
    if prompt_l.contains(&path_l) {
        bonus = bonus.saturating_add(40);
    }
    if basename_l.len() >= 4 && prompt_l.contains(&basename_l) {
        bonus = bonus.saturating_add(18);
    }
    if stem_l.len() >= 4 && prompt_l.contains(&stem_l) {
        bonus = bonus.saturating_add(10);
    }
    bonus
}

fn related_file_summaries(index: &ProjectIndex, top_files: &[&IndexedFile]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = top_files.iter().map(|f| f.path.clone()).collect();

    // Walk the dependency graph: 1-hop neighbours (importers or imported)
    // of each top file. Falls back to sibling/import-string heuristic when
    // the graph is empty (e.g. when an older index.json without graph
    // data is loaded).
    let graph_empty = index.graph.edges.is_empty() && index.graph.reverse.is_empty();
    if !graph_empty {
        for top in top_files.iter().take(5) {
            for neighbour_path in index.graph.neighbours(&top.path) {
                if seen.contains(neighbour_path) {
                    continue;
                }
                if let Some(neighbour) = index.files.iter().find(|f| &f.path == neighbour_path) {
                    seen.insert(neighbour.path.clone());
                    out.push(neighbour.summary.clone());
                    if out.len() >= 10 {
                        return out;
                    }
                }
            }
        }
    }

    // If we still have room (or graph was empty), fall back to the
    // legacy sibling/import-string heuristic so old index.json files
    // and projects without resolvable imports still surface related
    // files.
    if out.len() < 10 {
        for top in top_files.iter().take(5) {
            let top_stem = path_stem(&top.path);
            let top_dir = top.path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");

            for candidate in &index.files {
                if seen.contains(candidate.path.as_str()) {
                    continue;
                }
                let candidate_stem = path_stem(&candidate.path);
                let candidate_dir = candidate
                    .path
                    .rsplit_once('/')
                    .map(|(dir, _)| dir)
                    .unwrap_or("");
                let top_imports_candidate =
                    imports_reference(&top.imports, &candidate.path, &candidate_stem);
                let candidate_imports_top =
                    imports_reference(&candidate.imports, &top.path, &top_stem);
                let sibling_support = !top_dir.is_empty()
                    && top_dir == candidate_dir
                    && matches!(candidate.role.as_str(), "test" | "config" | "documentation");

                if top_imports_candidate || candidate_imports_top || sibling_support {
                    seen.insert(candidate.path.clone());
                    out.push(candidate.summary.clone());
                    if out.len() >= 10 {
                        return out;
                    }
                }
            }
        }
    }
    out
}

fn path_stem(path: &str) -> String {
    let basename = path.rsplit('/').next().unwrap_or(path);
    basename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(basename)
        .to_lowercase()
}

fn imports_reference(imports: &[String], path: &str, stem: &str) -> bool {
    if stem.len() < 3 {
        return false;
    }
    let path_l = path.to_lowercase();
    imports.iter().any(|import| {
        let import_l = import.to_lowercase();
        import_l.contains(stem)
            || path_l.ends_with(&format!("{import_l}.ts"))
            || path_l.ends_with(&format!("{import_l}.tsx"))
            || path_l.ends_with(&format!("{import_l}.rs"))
    })
}

fn prompt_has_repo_marker(prompt: &str) -> bool {
    contains_any(
        prompt,
        &[
            "this repo",
            "this project",
            "existing",
            "current app",
            "our app",
            "our project",
            "codebase",
            "code base",
            "in the app",
            "in this app",
            "in our app",
            "file",
            "component",
            "function",
            "endpoint",
            "route",
            "test",
            "refactor",
            "frontend/",
            "backend/",
            "src/",
            ".tsx",
            ".ts",
            ".rs",
            ".py",
            ".go",
        ],
    )
}

fn query_terms(prompt: &str) -> HashSet<String> {
    let mut terms = prompt
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() >= 3)
        .collect::<HashSet<_>>();
    enrich_query_terms(prompt, &mut terms);
    terms
}

fn enrich_query_terms(prompt: &str, terms: &mut HashSet<String>) {
    let prompt_l = prompt.to_lowercase();
    let mut add_group = |needles: &[&str], additions: &[&str]| {
        if needles
            .iter()
            .any(|needle| terms.contains(*needle) || prompt_l.contains(needle))
        {
            terms.extend(additions.iter().map(|term| term.to_string()));
        }
    };

    add_group(
        &[
            "upload",
            "download",
            "drop",
            "drag",
            "file picker",
            "picker",
            "transfer",
        ],
        &[
            "sftp",
            "upload",
            "download",
            "drag",
            "drop",
            "file",
            "picker",
            "transfer",
            "clipboard",
            "remote",
            "local",
            "path",
            "progress",
        ],
    );
    add_group(
        &[
            "terminal",
            "shell",
            "pty",
            "xterm",
            "input",
            "prompt",
            "claude",
            "codex",
            "cursor",
            "backspace",
            "enter",
            "emoji",
            "ansi",
        ],
        &[
            "terminal", "shell", "pty", "xterm", "input", "prompt", "cursor", "keyboard", "ansi",
            "unicode", "resize",
        ],
    );
    add_group(
        &["ssh", "vps", "host", "known host", "key", "proxy", "tunnel"],
        &[
            "ssh",
            "host",
            "session",
            "known",
            "fingerprint",
            "key",
            "proxy",
            "tunnel",
            "connect",
            "reconnect",
        ],
    );
    add_group(
        &[
            "collaborator",
            "collab",
            "split",
            "multi terminal",
            "multiple terminal",
        ],
        &[
            "collaborator",
            "collab",
            "webview",
            "tile",
            "grid",
            "terminal",
            "session",
            "layout",
        ],
    );
    add_group(
        &[
            "theme",
            "team",
            "drawer",
            "sidebar",
            "appearance",
            "color",
            "font",
        ],
        &[
            "theme",
            "appearance",
            "drawer",
            "sidebar",
            "color",
            "font",
            "style",
            "css",
        ],
    );
    add_group(
        &[
            "vault",
            "keychain",
            "password",
            "secret",
            "credential",
            "encrypt",
        ],
        &[
            "vault",
            "keychain",
            "password",
            "secret",
            "credential",
            "crypto",
            "encrypt",
        ],
    );
    add_group(
        &["slow", "freeze", "lag", "hang", "performance", "jank"],
        &[
            "performance",
            "batch",
            "buffer",
            "debounce",
            "throttle",
            "async",
            "event",
            "render",
        ],
    );
}

/// Build TF-IDF corpus statistics over indexed files. Each "document" is
/// the concatenation of a file's summary + symbols + imports + chunk
/// keywords (NOT raw chunk text — keeps the corpus compact and aligns
/// with what retrieval matches against).
fn build_corpus_stats(files: &[IndexedFile]) -> CorpusStats {
    let doc_count = files.len();
    if doc_count == 0 {
        return CorpusStats::default();
    }
    let mut df: HashMap<String, usize> = HashMap::new();
    for file in files {
        let doc = doc_tokens(file);
        let mut seen = HashSet::new();
        for tok in doc {
            if seen.insert(tok.clone()) {
                *df.entry(tok).or_insert(0) += 1;
            }
        }
    }
    // Smoothed IDF (matches sklearn's default): log((N+1) / (df+1)) + 1.
    let n = doc_count as f32;
    let idf = df
        .into_iter()
        .map(|(tok, freq)| {
            let value = ((n + 1.0) / (freq as f32 + 1.0)).ln() + 1.0;
            (tok, value)
        })
        .collect();
    CorpusStats { idf, doc_count }
}

/// Tokens used for TF-IDF document representation. Reuses the same
/// alphanumeric + underscore split as `query_terms` so the index and
/// query speak the same language.
fn doc_tokens(file: &IndexedFile) -> Vec<String> {
    let mut buf = String::new();
    buf.push_str(&file.path);
    buf.push(' ');
    buf.push_str(&file.summary);
    buf.push(' ');
    for s in &file.symbols {
        buf.push_str(s);
        buf.push(' ');
    }
    for i in &file.imports {
        buf.push_str(i);
        buf.push(' ');
    }
    for chunk in &file.chunks {
        for kw in &chunk.keywords {
            buf.push_str(kw);
            buf.push(' ');
        }
    }
    buf.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() >= 3)
        .collect()
}

/// Compute a per-file TF for a given set of query terms.
fn term_frequency(file: &IndexedFile, terms: &HashSet<String>) -> HashMap<String, usize> {
    let tokens = doc_tokens(file);
    let mut tf: HashMap<String, usize> = HashMap::new();
    for tok in tokens {
        if terms.contains(&tok) {
            *tf.entry(tok).or_insert(0) += 1;
        }
    }
    tf
}

/// Parse the per-language import lists into a project-relative dependency
/// graph. Only resolves edges that point at known project files.
fn build_dependency_graph(files: &[IndexedFile]) -> DependencyGraph {
    let all_paths: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
    let mut graph = DependencyGraph::default();
    for file in files {
        let mut resolved = Vec::new();
        for import in &file.imports {
            if let Some(target) =
                resolve::resolve_import(&file.path, import, &file.language, &all_paths)
            {
                if target != file.path {
                    resolved.push(target.clone());
                    graph
                        .reverse
                        .entry(target)
                        .or_default()
                        .push(file.path.clone());
                }
            }
        }
        resolved.sort();
        resolved.dedup();
        if !resolved.is_empty() {
            graph.edges.insert(file.path.clone(), resolved);
        }
    }
    // Dedup the reverse adjacency lists too.
    for list in graph.reverse.values_mut() {
        list.sort();
        list.dedup();
    }
    graph
}

/// Build embedding vectors for every indexed file using the supplied
/// embedding provider. Falls back to skipping files whose embedding
/// payload would be empty (e.g. no summary text).
async fn embed_files(
    ai: &EmbeddingConfig,
    files: &[IndexedFile],
    prior_index: Option<&ProjectIndex>,
) -> AppResult<EmbeddingStore> {
    ai.validate()?;
    let prior_store = prior_index.and_then(|idx| idx.embeddings.as_ref());
    let prior_files = prior_file_map(prior_index);
    let can_reuse_prior = prior_store
        .map(|store| store.model == ai.embedding_model && store.provider == ai.provider)
        .unwrap_or(false);
    let mut file_vectors = HashMap::new();
    let mut payloads: Vec<(String, String)> = Vec::new();
    for file in files {
        if can_reuse_prior {
            if let (Some(prior), Some(store)) = (prior_files.get(file.path.as_str()), prior_store) {
                let hash_matches =
                    !prior.content_hash.is_empty() && prior.content_hash == file.content_hash;
                let legacy_matches = prior.content_hash.is_empty()
                    && prior.size == file.size
                    && prior.modified == file.modified;
                if hash_matches || legacy_matches {
                    if let Some(vector) = store.file_vectors.get(&file.path) {
                        file_vectors.insert(file.path.clone(), vector.clone());
                        continue;
                    }
                }
            }
        }
        let payload = file_embedding_payload(file);
        if payload.trim().is_empty() {
            continue;
        }
        payloads.push((file.path.clone(), payload));
    }
    if payloads.is_empty() {
        return Ok(EmbeddingStore {
            model: ai.embedding_model.clone(),
            provider: ai.provider.clone(),
            file_vectors,
            stale_since: None,
        });
    }
    let texts: Vec<String> = payloads.iter().map(|(_, t)| t.clone()).collect();
    let vectors = embed::embed_all(ai, texts).await?;
    for ((path, _), vector) in payloads.into_iter().zip(vectors.into_iter()) {
        file_vectors.insert(path, vector);
    }
    Ok(EmbeddingStore {
        model: ai.embedding_model.clone(),
        provider: ai.provider.clone(),
        file_vectors,
        stale_since: None,
    })
}

/// Compact text payload sent to the embedding API per file. Stays small
/// to keep cost predictable (~200 tokens per file).
fn file_embedding_payload(file: &IndexedFile) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("File: {} ({})\n", file.path, file.language));
    if !file.role.is_empty() {
        buf.push_str(&format!("Role: {}\n", file.role));
    }
    if !file.summary.is_empty() {
        buf.push_str(&format!("Summary: {}\n", file.summary));
    }
    if !file.symbols.is_empty() {
        buf.push_str("Symbols: ");
        buf.push_str(&file.symbols.join(", "));
        buf.push('\n');
    }
    if !file.imports.is_empty() {
        buf.push_str("Imports: ");
        buf.push_str(&file.imports.join(", "));
        buf.push('\n');
    }
    buf.chars().take(2_000).collect()
}

/// New scoring function that combines TF-IDF + n-gram similarity + the
/// existing path/symbol heuristic + hot-file lineage boost. Returns a
/// score in roughly the same shape as the legacy `score_file` (usize)
/// so downstream sort logic stays unchanged.
#[allow(clippy::too_many_arguments)]
fn score_file_v2(
    file: &IndexedFile,
    terms: &HashSet<String>,
    query_ngrams: &HashSet<String>,
    corpus_stats: &CorpusStats,
    lineage: Option<&Lineage>,
    intent: PromptIntent,
    now: i64,
    embeddings: Option<&EmbeddingStore>,
    query_vector: Option<&[f32]>,
) -> usize {
    if terms.is_empty() && query_ngrams.is_empty() && query_vector.is_none() {
        return 0;
    }
    let tf = term_frequency(file, terms);
    let mut tfidf: f32 = 0.0;
    for (term, count) in &tf {
        let idf = corpus_stats.idf.get(term).copied().unwrap_or(1.0);
        // Sub-linear TF (BM25-style) to stop a single high-count term
        // dominating.
        let normalized_tf = 1.0 + (*count as f32).ln();
        tfidf += normalized_tf * idf;
    }
    // n-gram similarity against the file's "identifier signature" — path
    // + role + symbols + imports. Skips file content so we don't reward
    // arbitrary text matches.
    let identifier_sig = format!(
        "{} {} {} {}",
        file.path,
        file.role,
        file.symbols.join(" "),
        file.imports.join(" ")
    );
    let target_ngrams = ngram::ngrams(&identifier_sig);
    let ngram_score = ngram::jaccard(query_ngrams, &target_ngrams);

    let heuristic = score_file(file, terms) as f32;

    // Optional semantic similarity from embeddings. Cosine ranges
    // [-1, 1] but for OpenAI-style models stays in [0, 1] for related
    // content.
    let embed_score = match (embeddings, query_vector) {
        (Some(store), Some(query_vec)) => store
            .file_vectors
            .get(&file.path)
            .map(|file_vec| embed::cosine_similarity(query_vec, file_vec).max(0.0))
            .unwrap_or(0.0),
        _ => 0.0,
    };

    // Blend the four signals. Weights tuned so TF-IDF dominates on
    // clear keyword hits and embeddings dominate on no-keyword
    // morphologically-distant queries (the "auth → login" case).
    let mut combined = if embed_score > 0.0 {
        0.4 * tfidf + 0.3 * (embed_score * 80.0) + 0.2 * (ngram_score * 50.0) + 0.1 * heuristic
    } else {
        0.6 * tfidf + 0.3 * (ngram_score * 50.0) + 0.1 * heuristic
    };

    // Hot-file lineage boost — only for intents where recent activity is
    // a meaningful signal.
    if matches!(
        intent,
        PromptIntent::BugFix | PromptIntent::Refactor | PromptIntent::Test
    ) {
        if let Some(lineage) = lineage {
            combined *= lineage.hotness_multiplier(&file.path, now);
        }
    }

    if combined.is_nan() || combined.is_sign_negative() {
        return 0;
    }
    combined.round() as usize
}

fn score_file(file: &IndexedFile, terms: &HashSet<String>) -> usize {
    let mut score = score_text(
        &format!(
            "{} {} {} {} {}",
            file.path,
            file.role,
            file.summary,
            file.symbols.join(" "),
            file.imports.join(" ")
        ),
        terms,
    );
    for term in terms {
        if file.path.to_lowercase().contains(term) {
            score = score.saturating_add(4);
        }
        if file.symbols.iter().any(|s| s.to_lowercase().contains(term)) {
            score = score.saturating_add(5);
        }
        if file.imports.iter().any(|s| s.to_lowercase().contains(term)) {
            score = score.saturating_add(3);
        }
    }
    score
}

fn score_text(text: &str, terms: &HashSet<String>) -> usize {
    if terms.is_empty() {
        return 0;
    }
    let lower = text.to_lowercase();
    terms.iter().map(|t| lower.matches(t).count()).sum()
}

fn top_keywords(text: &str, limit: usize) -> Vec<String> {
    let stop = [
        "the",
        "and",
        "for",
        "with",
        "this",
        "that",
        "from",
        "into",
        "const",
        "let",
        "pub",
        "use",
        "return",
        "async",
        "await",
        "function",
        "class",
        "struct",
        "impl",
        "true",
        "false",
        "null",
        "undefined",
    ];
    let stop = stop.into_iter().collect::<HashSet<_>>();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let word = word.trim().to_lowercase();
        if word.len() < 4 || stop.contains(word.as_str()) || word.chars().all(|c| c.is_numeric()) {
            continue;
        }
        *counts.entry(word).or_default() += 1;
    }
    let mut counts = counts.into_iter().collect::<Vec<_>>();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts.into_iter().take(limit).map(|(k, _)| k).collect()
}

fn is_manifest(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "package.json"
            | "Cargo.toml"
            | "pyproject.toml"
            | "go.mod"
            | "Package.swift"
            | "pom.xml"
            | "build.gradle"
            | "composer.json"
            | "Gemfile"
    )
}

fn is_config_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".config.ts")
        || name.ends_with(".config.js")
        || name.ends_with(".config.mjs")
        || name.ends_with(".config.cjs")
        || matches!(
            name,
            "tsconfig.json"
                | "vite.config.ts"
                | "vite.config.js"
                | "next.config.js"
                | "next.config.mjs"
                | "tailwind.config.js"
                | "tailwind.config.ts"
                | "tauri.conf.json"
                | "Cargo.lock"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | ".npmrc"
        )
}

fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/__tests__/")
        || lower.contains("/tests/")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".spec.ts")
        || lower.ends_with(".spec.tsx")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
}

fn looks_like_entrypoint(path: &str, text: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "main.rs"
            | "lib.rs"
            | "main.ts"
            | "main.tsx"
            | "index.ts"
            | "index.tsx"
            | "App.tsx"
            | "app.tsx"
            | "main.py"
            | "server.ts"
            | "server.js"
    ) || text.contains("createRoot(")
        || text.contains("tauri::Builder")
        || text.contains("#[tokio::main]")
}

fn detect_package_manager(path: &str, out: &mut Vec<String>) {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "package.json" => out.push("npm/js".to_string()),
        "Cargo.toml" => out.push("cargo/rust".to_string()),
        "pyproject.toml" => out.push("python".to_string()),
        "go.mod" => out.push("go".to_string()),
        "Package.swift" => out.push("swiftpm".to_string()),
        "composer.json" => out.push("composer/php".to_string()),
        "Gemfile" => out.push("bundler/ruby".to_string()),
        _ => {}
    }
}

fn detect_frameworks(file: &IndexedFile, out: &mut Vec<String>) {
    let text = format!(
        "{} {} {}",
        file.path.to_lowercase(),
        file.summary.to_lowercase(),
        file.imports.join(" ").to_lowercase()
    );
    for (needle, label) in [
        ("tauri", "Tauri"),
        ("react", "React"),
        ("vite", "Vite"),
        ("next", "Next.js"),
        ("xterm", "xterm.js"),
        ("tokio", "Tokio"),
        ("serde", "Serde"),
        ("swiftui", "SwiftUI"),
        ("expo", "Expo"),
        ("react-native", "React Native"),
        ("tailwind", "Tailwind"),
        ("svelte", "Svelte"),
        ("vue", "Vue"),
        ("django", "Django"),
        ("fastapi", "FastAPI"),
        ("laravel", "Laravel"),
    ] {
        if text.contains(needle) {
            out.push(label.to_string());
        }
    }
}

fn detect_capabilities(file: &IndexedFile, out: &mut Vec<String>) {
    let text = format!(
        "{} {} {} {} {}",
        file.path.to_lowercase(),
        file.role.to_lowercase(),
        file.summary.to_lowercase(),
        file.symbols.join(" ").to_lowercase(),
        file.imports.join(" ").to_lowercase()
    );
    for (needles, label) in [
        (
            &[
                "ssh",
                "known_host",
                "fingerprint",
                "host key",
                "proxy",
                "tunnel",
            ][..],
            "SSH sessions",
        ),
        (
            &["sftp", "upload", "download", "remote path", "transfer"][..],
            "SFTP file transfer",
        ),
        (
            &["terminal", "xterm", "pty", "shell", "ansi", "unicode"][..],
            "terminal rendering/input",
        ),
        (
            &[
                "collaborator",
                "collab",
                "tile",
                "multi terminal",
                "webview",
            ][..],
            "collaborator terminals",
        ),
        (
            &["vault", "keychain", "encrypt", "credential", "password"][..],
            "encrypted vault/credentials",
        ),
        (
            &[
                "prompt_enhance",
                "prompt enhancer",
                "brain",
                "project index",
            ][..],
            "prompt enhancement/project context",
        ),
        (
            &["theme", "appearance", "drawer", "font", "palette"][..],
            "themes/appearance",
        ),
        (
            &["test", "spec", "vitest", "pytest", "cargo test"][..],
            "tests/verification",
        ),
    ] {
        if needles.iter().any(|needle| text.contains(needle)) {
            out.push(label.to_string());
        }
    }
}

fn infer_architecture(files: &[IndexedFile], map: &ProjectMap) -> Vec<String> {
    let mut out = Vec::new();
    let has = |prefix: &str| files.iter().any(|file| file.path.starts_with(prefix));
    let has_path = |needle: &str| files.iter().any(|file| file.path.contains(needle));
    let has_framework = |name: &str| map.frameworks.iter().any(|fw| fw == name);

    if has("frontend/src/") || has("src/") && (has_framework("React") || has_framework("Vite")) {
        out.push("renderer UI layer".to_string());
    }
    if has("backend/src/") || has_framework("Tauri") {
        out.push("Rust/Tauri command backend".to_string());
    }
    if has_path("/components/") || has_path("components/") {
        out.push("component-driven UI".to_string());
    }
    if has_path("sftp") {
        out.push("SFTP service and transfer workflow".to_string());
    }
    if has_path("ssh") {
        out.push("SSH session/service layer".to_string());
    }
    if has_path("vault") || has_path("crypto") || has_path("keychain") {
        out.push("encrypted storage and credential layer".to_string());
    }
    if has_path("brain") || has_path("promptEnhancer") || has_path("prompt_enhance") {
        out.push("AI prompt-enhancer context engine".to_string());
    }
    if has_path("tests/") || files.iter().any(|file| file.role == "test") {
        out.push("test suite present".to_string());
    }
    if !map.manifests.is_empty() {
        out.push(format!("manifests: {}", list_or_none(&map.manifests)));
    }

    out.sort();
    out.dedup();
    out.truncate(16);
    out
}

fn extract_manifest_dependencies(file: &IndexedFile, deps: &mut HashSet<String>) {
    if file.path.ends_with("package.json") {
        for chunk in &file.chunks {
            for line in chunk.text.lines() {
                let t = line.trim();
                if !t.starts_with('"') || !t.contains(':') {
                    continue;
                }
                let Some(end) = t[1..].find('"') else {
                    continue;
                };
                let name = &t[1..1 + end];
                if name.len() >= 2
                    && !matches!(
                        name,
                        "scripts" | "dependencies" | "devDependencies" | "name" | "version"
                    )
                {
                    deps.insert(name.to_string());
                }
            }
        }
    } else if file.path.ends_with("Cargo.toml") {
        for chunk in &file.chunks {
            for line in chunk.text.lines() {
                let t = line.trim();
                if t.starts_with('[') || t.starts_with('#') || !t.contains('=') {
                    continue;
                }
                let name = t.split('=').next().unwrap_or("").trim();
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    deps.insert(name.to_string());
                }
            }
        }
    } else if file.path.ends_with("go.mod") {
        for chunk in &file.chunks {
            for line in chunk.text.lines() {
                let t = line.trim();
                if t.contains('/') && !t.starts_with("module ") {
                    deps.insert(t.split_whitespace().next().unwrap_or(t).to_string());
                }
            }
        }
    }
}

fn extract_project_scripts(file: &IndexedFile, scripts: &mut HashSet<String>) {
    if file.path.ends_with("package.json") {
        let text = file
            .chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(map) = json.get("scripts").and_then(|v| v.as_object()) {
                for (name, command) in map.iter().take(30) {
                    if let Some(command) = command.as_str() {
                        let clean = command.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !clean.is_empty() {
                            scripts.insert(format!("npm run {name}: {clean}"));
                        }
                    }
                }
            }
        }
    } else if file.path.ends_with("Cargo.toml") {
        scripts.insert("cargo check".to_string());
        scripts.insert("cargo test".to_string());
        scripts.insert("cargo build".to_string());
    } else if file.path.ends_with("pyproject.toml") {
        scripts.insert("python -m pytest".to_string());
    } else if file.path.ends_with("go.mod") {
        scripts.insert("go test ./...".to_string());
        scripts.insert("go build ./...".to_string());
    } else if file.path.ends_with("Package.swift") {
        scripts.insert("swift test".to_string());
        scripts.insert("swift build".to_string());
    } else if file.path.ends_with("composer.json") {
        scripts.insert("composer test".to_string());
    } else if file.path.ends_with("Gemfile") {
        scripts.insert("bundle exec rake test".to_string());
    }
}

fn modified_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn shell_quote(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_file(path: &str, text: &str) -> IndexedFile {
        IndexedFile {
            path: path.to_string(),
            language: language_for_path(Path::new(path)).to_string(),
            size: text.len() as u64,
            modified: 0,
            content_hash: content_hash(text.as_bytes()),
            role: infer_role(path, language_for_path(Path::new(path)), text),
            imports: Vec::new(),
            symbols: Vec::new(),
            summary: format!("{path} test summary"),
            chunks: vec![IndexChunk {
                start_line: 1,
                end_line: text.lines().count().max(1),
                keywords: top_keywords(text, 16),
                text: text.to_string(),
            }],
        }
    }

    fn project_index(files: Vec<IndexedFile>) -> ProjectIndex {
        let project_map = build_project_map(&files);
        let chunks_indexed = files.iter().map(|file| file.chunks.len()).sum();
        let total_bytes = files.iter().map(|file| file.size).sum();
        let corpus_stats = build_corpus_stats(&files);
        let graph = build_dependency_graph(&files);
        ProjectIndex {
            id: BrainId("test".to_string()),
            label: "test".to_string(),
            indexed_at: 0,
            root_hint: "/tmp/test".to_string(),
            files_indexed: files.len(),
            chunks_indexed,
            total_bytes,
            overview: build_overview("test", &project_map, &files),
            project_digest: build_project_digest("test", &project_map, &files),
            project_map,
            files,
            corpus_stats,
            graph,
            lineage: None,
            embeddings: None,
        }
    }

    #[test]
    fn project_context_is_skipped_for_fresh_build_prompts() {
        assert!(!should_use_project_context(
            "make a food delivery app with customer account, vendor account, and admin panel"
        ));
        assert!(!should_use_project_context(
            "build me a brand new dashboard from scratch"
        ));
    }

    #[test]
    fn project_context_is_kept_for_existing_repo_prompts() {
        assert!(should_use_project_context(
            "in this repo build a better landing page"
        ));
        assert!(should_use_project_context(
            "fix the dashboard bug in the current app"
        ));
    }

    #[test]
    fn index_text_file_reuses_unchanged_prior_file_by_hash() {
        let prior = indexed_file("src/lib.rs", "pub fn old_summary_target() {}");
        let reused = index_text_file(
            "src/lib.rs",
            prior.size,
            prior.modified + 10,
            "pub fn old_summary_target() {}",
            prior.content_hash.clone(),
            Some(&prior),
        );

        assert_eq!(reused.summary, prior.summary);
        assert_eq!(reused.modified, prior.modified + 10);
        assert_eq!(reused.content_hash, prior.content_hash);
    }

    #[test]
    fn index_text_file_reprocesses_changed_prior_file() {
        let prior = indexed_file("src/lib.rs", "pub fn old_symbol() {}");
        let changed = "pub fn new_symbol() {}";
        let rebuilt = index_text_file(
            "src/lib.rs",
            changed.len() as u64,
            prior.modified + 10,
            changed,
            content_hash(changed.as_bytes()),
            Some(&prior),
        );

        assert_ne!(rebuilt.content_hash, prior.content_hash);
        assert!(rebuilt
            .chunks
            .iter()
            .any(|chunk| chunk.text.contains("new_symbol")));
    }

    #[test]
    fn prompt_context_decision_classifies_common_intents() {
        let fresh = decide_project_context(
            "make a food delivery app with customer account vendor account and admin",
        );
        assert_eq!(fresh.kind, PromptIntentKind::FreshBuild);
        assert!(!fresh.use_project_context);

        let bug = decide_project_context("the SFTP upload button is broken in this app");
        assert_eq!(bug.kind, PromptIntentKind::BugFix);
        assert!(bug.use_project_context);

        let repo_change = decide_project_context("update frontend/src/components/Drawer.tsx");
        assert_eq!(repo_change.kind, PromptIntentKind::RepoChange);
        assert!(repo_change.use_project_context);

        let planning = decide_project_context("what do you think about a prompt enhancer mode?");
        assert_eq!(planning.kind, PromptIntentKind::Planning);
        assert!(!planning.use_project_context);
    }

    #[test]
    fn fresh_build_with_questions_still_skips_project_context() {
        let decision = decide_project_context(
            "make a food delivery app with customer account, vendor account, and admin panel. questions?",
        );

        assert_eq!(decision.kind, PromptIntentKind::FreshBuild);
        assert!(!decision.use_project_context);
    }

    #[test]
    fn repo_specific_questions_use_project_context() {
        let decision = decide_project_context(
            "For this repo, replace the SFTP upload flow with progress reporting. questions?",
        );

        assert_eq!(decision.kind, PromptIntentKind::RepoChange);
        assert!(decision.use_project_context);
    }

    #[test]
    fn project_map_extracts_package_json_scripts() {
        let package_json = r#"{
  "name": "tersh-test",
  "scripts": {
    "type-check": "tsc --noEmit",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "19.0.0"
  }
}"#;
        let map = build_project_map(&[indexed_file("package.json", package_json)]);

        assert!(map.manifests.contains(&"package.json".to_string()));
        assert!(map
            .scripts
            .contains(&"npm run type-check: tsc --noEmit".to_string()));
        assert!(map
            .scripts
            .contains(&"npm run test: vitest run".to_string()));
    }

    #[test]
    fn project_map_extracts_module_shape() {
        let files = vec![
            indexed_file(
                "frontend/src/components/Drawer.tsx",
                "export function Drawer() { return null }",
            ),
            indexed_file(
                "frontend/src/components/TerminalView.tsx",
                "export function TerminalView() { return null }",
            ),
            indexed_file("backend/src/commands.rs", "pub fn prompt_enhance() {}"),
            indexed_file("backend/src/brain/index.rs", "pub fn build_local() {}"),
            indexed_file("docs/prompt-enhancer.md", "# Prompt enhancer"),
        ];
        let map = build_project_map(&files);

        assert!(map
            .modules
            .iter()
            .any(|module| module.starts_with("frontend/src")));
        assert!(map
            .modules
            .iter()
            .any(|module| module.starts_with("backend/src")));
        assert!(map.modules.iter().any(|module| module.starts_with("docs")));
    }

    #[test]
    fn project_map_extracts_capabilities_and_architecture() {
        let mut terminal = indexed_file(
            "frontend/src/components/TerminalView.tsx",
            "import { Terminal } from 'xterm'; export function TerminalView() { return xterm.onData(handleKeyboardInput) }",
        );
        terminal.imports = vec!["xterm".to_string()];

        let files = vec![
            terminal,
            indexed_file(
                "backend/src/ssh/mod.rs",
                "pub async fn connect_ssh_session() { /* host key fingerprint */ }",
            ),
            indexed_file(
                "backend/src/sftp/mod.rs",
                "pub async fn upload_to_path() { /* sftp upload transfer */ }",
            ),
            indexed_file(
                "backend/src/brain/index.rs",
                "pub fn build_project_index() { /* prompt enhancer project index */ }",
            ),
        ];
        let map = build_project_map(&files);

        assert!(map
            .capabilities
            .contains(&"terminal rendering/input".to_string()));
        assert!(map.capabilities.contains(&"SSH sessions".to_string()));
        assert!(map.capabilities.contains(&"SFTP file transfer".to_string()));
        assert!(map
            .capabilities
            .contains(&"prompt enhancement/project context".to_string()));
        assert!(map
            .architecture
            .contains(&"Rust/Tauri command backend".to_string()));
        assert!(map
            .architecture
            .contains(&"AI prompt-enhancer context engine".to_string()));
    }

    #[test]
    fn project_map_extracts_cargo_verification_scripts() {
        let cargo_toml = r#"[package]
name = "tersh-test"
version = "0.1.0"

[dependencies]
serde = "1"
"#;
        let map = build_project_map(&[indexed_file("Cargo.toml", cargo_toml)]);

        assert!(map.scripts.contains(&"cargo check".to_string()));
        assert!(map.scripts.contains(&"cargo test".to_string()));
        assert!(map.scripts.contains(&"cargo build".to_string()));
    }

    #[test]
    fn project_digest_summarizes_project_brain_signals() {
        let package_json = r#"{
  "name": "tersh-test",
  "scripts": {
    "type-check": "tsc --noEmit",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "19.0.0",
    "@xterm/xterm": "5.5.0"
  }
}"#;
        let files = vec![
            indexed_file("package.json", package_json),
            indexed_file(
                "frontend/src/components/TerminalView.tsx",
                "import { Terminal } from '@xterm/xterm'; export function TerminalView() { return null }",
            ),
            indexed_file(
                "backend/src/brain/index.rs",
                "pub fn build_project_index() { /* prompt enhancer project index */ }",
            ),
        ];
        let map = build_project_map(&files);
        let digest = build_project_digest("tersh-test", &map, &files);

        assert!(digest.contains("Project digest for tersh-test"));
        assert!(digest.contains("frontend/src"));
        assert!(digest.contains("backend/src"));
        assert!(digest.contains("prompt enhancement/project context"));
        assert!(digest.contains("npm run type-check: tsc --noEmit"));
        assert!(digest.contains("@xterm/xterm"));
    }

    #[test]
    fn query_terms_expand_domain_language() {
        let upload_terms = query_terms("file picker upload failed after drag");
        assert!(upload_terms.contains("sftp"));
        assert!(upload_terms.contains("progress"));
        assert!(upload_terms.contains("remote"));

        let terminal_terms = query_terms("Claude input cursor is broken in the terminal");
        assert!(terminal_terms.contains("xterm"));
        assert!(terminal_terms.contains("pty"));
        assert!(terminal_terms.contains("keyboard"));

        let collaborator_terms = query_terms("collab multi terminal layout is buggy");
        assert!(collaborator_terms.contains("webview"));
        assert!(collaborator_terms.contains("tile"));
        assert!(collaborator_terms.contains("grid"));
    }

    #[test]
    fn context_retrieval_uses_expanded_terms_for_upload_prompts() {
        let index = project_index(vec![
            indexed_file(
                "frontend/src/components/SftpPage.tsx",
                "export function SftpPage() { return uploadFile().then(showProgress) }",
            ),
            indexed_file(
                "backend/src/sftp/mod.rs",
                "pub async fn upload_to_path() { /* remote file transfer */ }",
            ),
            indexed_file(
                "frontend/src/components/Drawer.tsx",
                "export function Drawer() { return appearancePanel() }",
            ),
        ]);

        let context = build_context(
            &index,
            "the file picker failed and upload has no progress",
            None,
        )
        .text;
        assert!(context.contains("frontend/src/components/SftpPage.tsx"));
        assert!(context.contains("backend/src/sftp/mod.rs"));
    }

    #[test]
    fn context_retrieval_uses_expanded_terms_for_terminal_prompts() {
        let index = project_index(vec![
            indexed_file(
                "frontend/src/components/TerminalView.tsx",
                "export function TerminalView() { xterm.onData(handleKeyboardInput) }",
            ),
            indexed_file(
                "backend/src/local_terminal.rs",
                "pub fn spawn_pty() { /* local shell */ }",
            ),
            indexed_file(
                "frontend/src/components/SftpPage.tsx",
                "export function SftpPage() {}",
            ),
        ]);

        let context = build_context(&index, "Claude input cursor glitches when I type", None).text;
        assert!(context.contains("frontend/src/components/TerminalView.tsx"));
        assert!(context.contains("backend/src/local_terminal.rs"));
    }

    #[test]
    fn context_retrieval_carries_project_map_not_just_file_hits() {
        let package_json = r#"{
  "name": "tersh-test",
  "scripts": {
    "type-check": "tsc --noEmit",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "19.0.0",
    "@xterm/xterm": "5.5.0"
  }
}"#;
        let index = project_index(vec![
            indexed_file("package.json", package_json),
            indexed_file(
                "frontend/src/components/TerminalView.tsx",
                "import { Terminal } from '@xterm/xterm'; export function TerminalView() { return null }",
            ),
            indexed_file(
                "backend/src/commands.rs",
                "pub async fn prompt_enhance() { /* prompt enhancer command */ }",
            ),
        ]);

        let context = build_context(
            &index,
            "in this repo improve the terminal prompt enhancer workflow",
            None,
        )
        .text;

        assert!(context.contains("Project map:"));
        assert!(context.contains("Stored project digest:"));
        assert!(context.contains("Agent rule: treat this digest as orientation"));
        assert!(context.contains("npm run type-check: tsc --noEmit"));
        assert!(context.contains("react"));
        assert!(context.contains("@xterm/xterm"));
        assert!(context.contains("frontend/src/components/TerminalView.tsx"));
        assert!(context.contains("backend/src/commands.rs"));
    }

    #[test]
    fn unmatched_repo_prompt_gets_digest_without_fake_file_matches() {
        let index = project_index(vec![
            indexed_file("package.json", r#"{"scripts":{"test":"vitest run"}}"#),
            indexed_file(
                "frontend/src/components/TerminalView.tsx",
                "export function TerminalView() {}",
            ),
        ]);

        let context = build_context(
            &index,
            "in this repo polish the quantum banana workflow",
            None,
        )
        .text;

        assert!(context.contains("Stored project digest:"));
        assert!(context.contains("No file was confidently matched by the index"));
        assert!(!context.contains("Most relevant files:"));
        assert!(!context.contains("Relevant indexed chunks:"));
    }

    /// Build an IndexedFile with explicit imports so dependency-graph tests
    /// have edges to walk. Mirrors `indexed_file` otherwise.
    fn indexed_file_with_imports(path: &str, text: &str, imports: Vec<&str>) -> IndexedFile {
        let mut file = indexed_file(path, text);
        file.imports = imports.into_iter().map(|s| s.to_string()).collect();
        file
    }

    fn top_selected_path(index: &ProjectIndex, prompt: &str) -> Option<String> {
        let ctx = build_context(index, prompt, None);
        ctx.trace
            .into_iter()
            .find(|t| t.tool == "index_file")
            .and_then(|t| t.target)
    }

    fn selected_paths(index: &ProjectIndex, prompt: &str) -> Vec<String> {
        build_context(index, prompt, None)
            .trace
            .into_iter()
            .filter(|t| t.tool == "index_file")
            .filter_map(|t| t.target)
            .collect()
    }

    // ─── Augment-parity benchmark fixture ──────────────────────────────
    // A miniature repo standing in for "tersh-like" Tauri app: SFTP
    // upload feature, terminal view, auth, prompt enhancer. Every test
    // below shares this fixture so the retrieval signal is constant.
    fn parity_fixture() -> ProjectIndex {
        project_index(vec![
            indexed_file(
                "frontend/src/components/SftpUploadButton.tsx",
                "import { uploadFile } from '../lib/sftpClient';\n\
                 export function SftpUploadButton(props: { sessionId: string }) {\n\
                   return <button onClick={() => uploadFile(props.sessionId)}>Upload to remote</button>;\n\
                 }",
            ),
            indexed_file_with_imports(
                "frontend/src/lib/sftpClient.ts",
                "export async function uploadFile(sessionId: string) {\n\
                   /* drag-drop SFTP upload helper */\n\
                 }",
                vec![],
            ),
            indexed_file(
                "frontend/src/components/TerminalView.tsx",
                "import { Terminal } from '@xterm/xterm';\n\
                 export function TerminalView() { return new Terminal(); }",
            ),
            indexed_file(
                "frontend/src/components/AuthDialog.tsx",
                "export function AuthDialog() { /* SSH key passphrase prompt */ return null; }",
            ),
            indexed_file(
                "backend/src/commands.rs",
                "pub async fn prompt_enhance() { /* prompt enhancer command */ }\n\
                 pub async fn sftp_upload() { /* sftp upload command */ }",
            ),
        ])
    }

    #[test]
    fn parity_sftp_prompt_ranks_sftp_files_above_unrelated_terminal() {
        let index = parity_fixture();
        let picks = selected_paths(&index, "fix the SFTP upload button in this repo");

        let top3 = picks.iter().take(3).cloned().collect::<Vec<_>>();
        assert!(
            top3.iter().any(|p| p.ends_with("SftpUploadButton.tsx")),
            "expected SftpUploadButton.tsx in top 3, got {top3:?}",
        );
        assert!(
            !top3.iter().any(|p| p.ends_with("AuthDialog.tsx")),
            "unrelated AuthDialog should NOT outrank SFTP files, got {top3:?}",
        );
    }

    #[test]
    fn parity_terminal_prompt_picks_terminal_view_first() {
        let index = parity_fixture();
        let top = top_selected_path(&index, "the xterm terminal view jitters when resizing")
            .expect("at least one match");
        assert!(top.ends_with("TerminalView.tsx"), "got {top}");
    }

    #[test]
    fn parity_prompt_enhancer_prompt_pulls_backend_commands() {
        let index = parity_fixture();
        let picks = selected_paths(&index, "improve the prompt enhancer agent loop");
        assert!(
            picks.iter().any(|p| p.ends_with("backend/src/commands.rs")),
            "prompt-enhancer prompt should pull commands.rs, got {picks:?}",
        );
    }

    // ─── n-gram fuzzy match: typo-tolerance ────────────────────────────
    #[test]
    fn ngram_matches_identifier_despite_typo() {
        // File is named after its main export so the identifier signature
        // (path + role + symbols + imports) carries "uploadFile" trigrams.
        // A typo'd query token ("uplaodFile") shares 3- to 5-grams with
        // "uploadFile" — Jaccard should keep this file in the top picks
        // even though no exact term matches.
        let index = project_index(vec![
            indexed_file(
                "frontend/src/lib/uploadFile.ts",
                "export async function uploadFile() {}",
            ),
            indexed_file(
                "frontend/src/components/AuthDialog.tsx",
                "export function AuthDialog() { return null; }",
            ),
        ]);
        let top =
            top_selected_path(&index, "in this repo fix uplaodFile").expect("at least one match");
        assert!(
            top.ends_with("uploadFile.ts"),
            "n-gram fuzzy match failed: got {top}",
        );
    }

    // ─── Dependency graph reverse walk ─────────────────────────────────
    #[test]
    fn dependency_graph_walks_reverse_imports() {
        // SftpUploadButton.tsx → imports ../lib/sftpClient
        // A prompt about sftpClient.ts should surface SftpUploadButton.tsx
        // as a related file because of the import edge.
        let files = vec![
            indexed_file_with_imports(
                "frontend/src/components/SftpUploadButton.tsx",
                "import { uploadFile } from '../lib/sftpClient';\n\
                 export function SftpUploadButton() { return null; }",
                vec!["../lib/sftpClient"],
            ),
            indexed_file_with_imports(
                "frontend/src/lib/sftpClient.ts",
                "export async function uploadFile() {}",
                vec![],
            ),
        ];
        let index = project_index(files);

        // sftpClient.ts has the reverse edge -> SftpUploadButton.tsx.
        let reverse = index
            .graph
            .neighbours("frontend/src/lib/sftpClient.ts")
            .collect::<Vec<_>>();
        assert!(
            reverse.iter().any(|p| p.contains("SftpUploadButton.tsx")),
            "reverse graph edge missing, got {reverse:?}",
        );

        // build_context for an sftpClient prompt should include the
        // related button file via graph walk.
        let ctx = build_context(&index, "rename the uploadFile helper in sftpClient", None);
        assert!(
            ctx.text.contains("SftpUploadButton.tsx"),
            "related files should pull SftpUploadButton.tsx via graph",
        );
    }

    // ─── Git lineage hot-file boost ────────────────────────────────────
    #[test]
    fn lineage_boosts_hot_file_for_bugfix_intent() {
        let mut index = project_index(vec![
            indexed_file(
                "frontend/src/components/SftpUploadButton.tsx",
                "export function SftpUploadButton() { return null; }",
            ),
            indexed_file(
                "frontend/src/lib/sftpClient.ts",
                "export async function uploadFile() {}",
            ),
        ]);

        // Stamp sftpClient.ts as touched 1 day ago — under the 30d hot
        // window, so the multiplier should kick in for BugFix intent.
        let now = crate::brain::unix_secs();
        let mut activity = HashMap::new();
        activity.insert(
            "frontend/src/lib/sftpClient.ts".to_string(),
            crate::brain::lineage::FileActivity {
                commits_touched: 3,
                last_touched_at: now - 60 * 60 * 24,
                recent_subjects: vec!["fix: upload retry".into()],
            },
        );
        index.lineage = Some(crate::brain::lineage::Lineage {
            recently_active: vec!["frontend/src/lib/sftpClient.ts".into()],
            file_activity: activity,
        });

        // Without lineage: both files are roughly tied for a generic
        // upload-failure prompt. With the hot-file boost, sftpClient.ts
        // (recently touched) should appear in the top picks for a bug-fix
        // prompt.
        let picks = selected_paths(&index, "this repo's sftp upload is broken, fix it");
        assert!(
            picks.iter().any(|p| p.ends_with("sftpClient.ts")),
            "hot file should make the cut, got {picks:?}",
        );
    }

    // ─── Embedding lifecycle: silent refresh preserves vectors ─────────
    #[tokio::test]
    async fn silent_refresh_carries_embeddings_forward_with_stale_stamp() {
        // Persist a prior index with embeddings, then call
        // carry_over_embeddings against an overlapping file set. The
        // returned store should retain only paths that still exist and
        // be stamped stale_since=now.
        let prior_id = BrainId("test-carryover".to_string());
        let mut prior_files = vec![
            indexed_file(
                "frontend/src/lib/sftpClient.ts",
                "export async function uploadFile() {}",
            ),
            indexed_file(
                "frontend/src/lib/deletedFile.ts",
                "// will be removed in the next build",
            ),
        ];
        let mut prior_store = EmbeddingStore {
            model: "openai/text-embedding-3-small".to_string(),
            provider: "openrouter".to_string(),
            file_vectors: HashMap::new(),
            stale_since: None,
        };
        prior_store
            .file_vectors
            .insert("frontend/src/lib/sftpClient.ts".into(), vec![0.1; 8]);
        prior_store
            .file_vectors
            .insert("frontend/src/lib/deletedFile.ts".into(), vec![0.2; 8]);

        let mut prior_index = project_index(prior_files.split_off(0));
        prior_index.id = prior_id.clone();
        prior_index.embeddings = Some(prior_store);
        write_index(&prior_index)
            .await
            .expect("persist prior index");

        // New build drops deletedFile.ts and adds AuthDialog.tsx.
        let new_files = vec![
            indexed_file(
                "frontend/src/lib/sftpClient.ts",
                "export async function uploadFile() {}",
            ),
            indexed_file(
                "frontend/src/components/AuthDialog.tsx",
                "export function AuthDialog() {}",
            ),
        ];
        let carried = carry_over_embeddings(&prior_id, &new_files)
            .await
            .expect("vectors should carry forward");

        // sftpClient.ts vector preserved.
        assert!(carried
            .file_vectors
            .contains_key("frontend/src/lib/sftpClient.ts"));
        // deletedFile.ts vector pruned.
        assert!(!carried
            .file_vectors
            .contains_key("frontend/src/lib/deletedFile.ts"));
        // New file has no vector (not embedded yet).
        assert!(!carried
            .file_vectors
            .contains_key("frontend/src/components/AuthDialog.tsx"));
        // Stamped as stale.
        assert!(
            carried.stale_since.is_some(),
            "carry-over must stamp stale_since"
        );
        assert_eq!(carried.model, "openai/text-embedding-3-small");
    }

    // ─── Embedding opt-in: when disabled, score still works ────────────
    #[test]
    fn retrieval_works_without_embeddings_block() {
        let index = parity_fixture();
        assert!(index.embeddings.is_none(), "default is no embeddings");
        // Sanity: TF-IDF + n-gram + path-mention combine to pick the
        // right file for a clear prompt — proving the embedding path is
        // genuinely OPTIONAL, not load-bearing.
        let top = top_selected_path(&index, "in this repo fix the SFTP upload button")
            .expect("non-empty");
        assert!(top.ends_with("SftpUploadButton.tsx"), "got {top}");
    }
}
