export type OsKind =
  | "ubuntu" | "debian" | "fedora" | "arch" | "alpine"
  | "centos" | "rhel" | "apple" | "windows" | "bsd" | "linux";

export interface HostRow {
  id: string;
  label: string;
  hostname: string;
  port: number;
  username: string;
  auth_kind: "password" | "key_file";
  key_path: string | null;
  group_name: string | null;
  os?: OsKind | null;
  jump_host_id?: string | null;
  env_json?: string | null;
  startup_snippet?: string | null;
}

export interface AddHostInput {
  label: string;
  hostname: string;
  port: number;
  username: string;
  auth_kind: "password" | "key_file";
  key_path: string | null;
  group_name: string | null;
  os: OsKind | null;
  jump_host_id?: string | null;
  env_json?: string | null;
  startup_snippet?: string | null;
}

export type AgentKind = "claude" | "aider" | "codex" | "gemini" | "cursor_agent";

export interface UploadResult {
  remote_path: string;
  bytes_written: number;
  formatted_for_agent: string;
  detected_agent: AgentKind | null;
  files_uploaded?: number;
}

export interface ConnectResponse {
  session_id: string;
}

export type SessionState =
  | { kind: "idle" }
  | { kind: "auth_needed"; reason: string }
  | { kind: "connecting" }
  | { kind: "connected"; sessionId: string }
  | {
      /** Connection dropped (network blip, server reboot, keepalive timeout).
       * Distinct from a user-initiated close — the tab stays open and an
       * auto-reconnect attempt is scheduled, with an exponential backoff. */
      kind: "reconnecting";
      attempt: number;
      nextRetryAt: number;
      lastSessionId: string;
    }
  | { kind: "error"; message: string };

export interface Tab {
  id: string;
  host: HostRow;
  state: SessionState;
  /** Session-scoped visual settings. Changing theme/font/size in one tab must not mutate sibling sessions. */
  appearance?: import("./lib/appearance").Appearance;
  /** "ssh" (default) = remote terminal, "sftp" = remote file browser, "local" = local shell. */
  kind?: "ssh" | "sftp" | "local";
  /** Last remote directory shown by an SFTP tab. */
  sftpCwd?: string;
  /** Live local terminal working directory. Updated when the shell changes directory. */
  localCwd?: string;
  /** Folder the terminal was originally opened for. Used as workspace context, not live shell state. */
  localStartCwd?: string;
  /** pending uploads that haven't been inserted into the terminal yet */
  pendingChip?: UploadResult;
}

export interface RemoteEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: number;
}

export interface SftpListing {
  cwd: string;
  entries: RemoteEntry[];
  truncated: boolean;
}

export interface RemoteFilePreview {
  path: string;
  bytes: number[];
}

export interface TransferProgress {
  transfer_id: string;
  path: string;
  bytes_done: number;
  total: number;
  done: boolean;
}

/** Live remote project-index progress, streamed over
 *  `brain://index/<id>/progress` while a VPS project is being indexed. */
export interface BrainIndexProgress {
  id: string;
  path: string;
  processed: number;
  total: number;
  done: boolean;
}

export type PromptEnhancerProvider = "openrouter" | "deepseek" | "mimo" | "custom";

export interface PromptEnhanceRequest {
  provider: PromptEnhancerProvider;
  base_url?: string | null;
  api_key: string;
  model: string;
  prompt: string;
  brain_id?: string | null;
  session_id?: string | null;
  /** Optional embedding model. Empty/null = TF-IDF + n-gram retrieval only.
   *  Never silently defaulted (CLAUDE.md). DeepSeek direct ignored. */
  embedding_model?: string | null;
}

export interface BrainIndexAiConfig {
  provider: PromptEnhancerProvider;
  base_url?: string | null;
  api_key: string;
  model: string;
  /** Same semantics as PromptEnhanceRequest.embedding_model. When set,
   *  index-time embedding pass runs against {base_url}/embeddings. */
  embedding_model?: string | null;
}

export interface PromptContextTraceItem {
  tool: string;
  target?: string | null;
  status: "ok" | "error" | string;
}

export type PromptIntentKind =
  | "fresh_build"
  | "repo_change"
  | "bug_fix"
  | "planning"
  | "research"
  | "question"
  | "unclear";

export interface PromptEnhanceResponse {
  enhanced_prompt: string;
  interpretation?: string | null;
  used_project_context: boolean;
  prompt_intent: PromptIntentKind;
  context_reason: string;
  project_context_available: boolean;
  provider: PromptEnhancerProvider;
  model: string;
  tool_calls_used: number;
  context_trace: PromptContextTraceItem[];
}

export type SidebarSection =
  | "hosts" | "sftp" | "keychain" | "tunnels"
  | "snippets" | "known-hosts";

export type DrawerTab = "snippets" | "history" | "enhancer" | "brain" | "appearance";

// ── PROJECT BRAIN ──────────────────────────────────────────────────────────
export type BrainScope =
  | { kind: "local"; root: string }
  | {
      kind: "remote";
      host_id: string;
      host_fingerprint: string;
      remote_root: string;
    };

export interface BrainStatus {
  id: string;
  label: string;
  scope: BrainScope;
  last_used_at: number;
  indexed_at: number;
  files_indexed: number;
  chunks_indexed: number;
  overview: string;
  project_digest: string;
  languages: string[];
  frameworks: string[];
  capabilities: string[];
  architecture: string[];
  modules: string[];
  /** True when an embeddings layer is present in the index. */
  has_embeddings?: boolean;
  /** Unix seconds at which embeddings first became stale — null when
   *  embeddings are absent / fresh. Indicates that a silent refresh
   *  couldn't recompute embeddings (no AI config in memory). */
  embeddings_stale_since?: number | null;
  /** Embedding model used to compute the persisted vectors. */
  embedding_model?: string;
}

export interface KeyRow {
  id: string;
  label: string;
  kind: string;
  public_key: string;
  fingerprint: string;
  private_path: string | null;
  created_at: number;
}

export interface GeneratedKey {
  id: string;
  public_key: string;
  fingerprint: string;
  private_path: string;
}

export interface SnippetRow {
  id: string;
  label: string;
  command: string;
  description: string | null;
  tags: string | null;
  group_path?: string | null;
  created_at: number;
}

export interface AddSnippetInput {
  label: string;
  command: string;
  description: string | null;
  tags: string | null;
  group_path?: string | null;
}

export interface KnownHostRow {
  host_id: string;
  fingerprint: string;
  first_seen: number;
}

export interface TunnelRow {
  id: string;
  host_id: string;
  label: string;
  kind: "local" | "remote" | "dynamic";
  local_port: number;
  remote_host: string | null;
  remote_port: number | null;
}

export interface AddTunnelInput {
  host_id: string;
  label: string;
  kind: "local" | "remote" | "dynamic";
  local_port: number;
  remote_host: string | null;
  remote_port: number | null;
}
