import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AddHostInput, ConnectResponse, HostRow, UploadResult,
  KeyRow, SnippetRow, AddSnippetInput, KnownHostRow,
  TunnelRow, AddTunnelInput, GeneratedKey,
  SftpListing, TransferProgress, RemoteFilePreview,
  PromptEnhanceRequest, PromptEnhanceResponse,
  BrainStatus, BrainIndexAiConfig, BrainIndexProgress,
} from "../types";

export const api = {
  listHosts: (): Promise<HostRow[]> => invoke("list_hosts"),
  addHost: (input: AddHostInput): Promise<string> => invoke("add_host", { input }),
  updateHost: (id: string, input: AddHostInput): Promise<void> => invoke("update_host", { id, input }),
  deleteHost: (id: string): Promise<void> => invoke("delete_host", { id }),

  connect: (req: {
    host_id: string;
    auth_secret: string | null;
    cols: number;
    rows: number;
    remember_key_passphrase?: boolean;
  }): Promise<ConnectResponse> => invoke("connect", { req }),

  startLocalTerminal: (cols: number, rows: number, cwd?: string | null): Promise<ConnectResponse> =>
    invoke("start_local_terminal", { cols, rows, cwd: cwd ?? null }),

  disconnect: (sessionId: string): Promise<void> =>
    invoke("disconnect", { sessionId }),

  sendInput: (sessionId: string, data: string): Promise<void> =>
    invoke("send_input", { sessionId, data }),

  sendInputRaw: (sessionId: string, data: Uint8Array): Promise<void> =>
    invoke("send_input_raw", data, { headers: { "x-session-id": sessionId } }),

  copyLocalImageToClipboard: (path: string): Promise<void> =>
    invoke("copy_local_image_to_clipboard", { path }),

  resize: (sessionId: string, cols: number, rows: number): Promise<void> =>
    invoke("resize_pty", { sessionId, cols, rows }),

  bindTerminalOutput: (
    sessionId: string,
    channel: Channel<ArrayBuffer | Uint8Array | number[]>,
  ): Promise<void> => invoke("bind_terminal_output", { sessionId, channel }),

  detectRemoteOs: (sessionId: string): Promise<HostRow["os"] | null> =>
    invoke("detect_remote_os", { sessionId }),

  uploadLocal: (sessionId: string, localPath: string, hostLabel: string, transferId?: string, preferredDir?: string | null): Promise<UploadResult> =>
    invoke("sftp_upload_local", { sessionId, localPath, hostLabel, transferId: transferId ?? null, preferredDir: preferredDir ?? null }),
  uploadFolderLocal: (sessionId: string, localPath: string, hostLabel: string, transferId?: string, preferredDir?: string | null): Promise<UploadResult> =>
    invoke("sftp_upload_folder_local", { sessionId, localPath, hostLabel, transferId: transferId ?? null, preferredDir: preferredDir ?? null }),

  onSessionOutput: (
    sessionId: string,
    onOut: (b64: string) => void,
    onErr: (b64: string) => void,
    onClose: () => void,
  ): Promise<UnlistenFn[]> =>
    Promise.all([
      listen<string>(`ssh://${sessionId}/out`, e => onOut(e.payload)),
      listen<string>(`ssh://${sessionId}/err`, e => onErr(e.payload)),
      listen<unknown>(`ssh://${sessionId}/close`, () => onClose()),
    ]),

  /// Fires only when the SSH connection drops UNEXPECTEDLY (server reboot,
  /// network partition, keepalive failure). Intentional user disconnects
  /// don't trigger this — only `ssh://<id>/close` fires for those. The tab
  /// state machine listens to this to start auto-reconnect with backoff.
  onSessionDisconnected: (sessionId: string, cb: () => void): Promise<UnlistenFn> =>
    listen<unknown>(`ssh://${sessionId}/disconnected`, () => cb()),

  onLocalTerminalCwd: (sessionId: string, cb: (cwd: string) => void): Promise<UnlistenFn> =>
    listen<string>(`local-terminal://${sessionId}/cwd`, e => cb(e.payload)),

  // ── keychain ──
  listKeys: (): Promise<KeyRow[]> => invoke("list_keys"),
  deleteKey: (id: string): Promise<void> => invoke("delete_key", { id }),
  generateKey: (label: string, comment?: string): Promise<GeneratedKey> =>
    invoke("generate_key", { input: { label, comment: comment ?? null } }),
  importKey: (label: string, path: string): Promise<KeyRow> =>
    invoke("import_key", { input: { label, path } }),

  // ── snippets ──
  listSnippets: (): Promise<SnippetRow[]> => invoke("list_snippets"),
  addSnippet: (input: AddSnippetInput): Promise<string> => invoke("add_snippet", { input }),
  updateSnippet: (id: string, input: AddSnippetInput): Promise<void> =>
    invoke("update_snippet", { id, input }),
  deleteSnippet: (id: string): Promise<void> => invoke("delete_snippet", { id }),
  runSnippet: (sessionId: string, snippetId: string): Promise<void> =>
    invoke("run_snippet", { sessionId, snippetId }),

  // ── known hosts ──
  listKnownHosts: (): Promise<KnownHostRow[]> => invoke("list_known_hosts"),

  // ── tunnels ──
  listTunnels: (): Promise<TunnelRow[]> => invoke("list_tunnels"),
  addTunnel: (input: AddTunnelInput): Promise<string> => invoke("add_tunnel", { input }),
  deleteTunnel: (id: string): Promise<void> => invoke("delete_tunnel", { id }),
  startTunnel: (tunnelId: string, sessionId: string): Promise<void> =>
    invoke("start_tunnel", { req: { tunnel_id: tunnelId, session_id: sessionId } }),
  stopTunnel: (tunnelId: string): Promise<void> => invoke("stop_tunnel", { tunnelId }),
  activeTunnels: (): Promise<string[]> => invoke("active_tunnels"),

  // ── SFTP CRUD ──
  sftpListRemote: (sessionId: string, path: string): Promise<SftpListing> =>
    invoke("sftp_list", { sessionId, path }),
  listLocalDir: (path: string): Promise<SftpListing> => invoke("list_local_dir", { path }),
  sftpDownload: (sessionId: string, remotePath: string, localPath: string, transferId?: string): Promise<number> =>
    invoke("sftp_download", { sessionId, remotePath, localPath, transferId: transferId ?? null }),
  sftpUploadTo: (sessionId: string, localPath: string, remotePath: string, transferId?: string): Promise<UploadResult> =>
    invoke("sftp_upload_to", { sessionId, localPath, remotePath, transferId: transferId ?? null }),
  sftpUploadFolderTo: (sessionId: string, localPath: string, remotePath: string, transferId?: string): Promise<UploadResult> =>
    invoke("sftp_upload_folder_to", { sessionId, localPath, remotePath, transferId: transferId ?? null }),
  sftpCancelTransfer: (transferId: string): Promise<boolean> =>
    invoke("sftp_cancel_transfer", { transferId }),
  onTransferProgress: (transferId: string, cb: (p: TransferProgress) => void): Promise<UnlistenFn> =>
    listen<TransferProgress>(`sftp://transfer/${transferId}/progress`, e => cb(e.payload)),
  sftpRename: (sessionId: string, from: string, to: string): Promise<void> =>
    invoke("sftp_rename", { sessionId, from, to }),
  sftpMkdir: (sessionId: string, path: string): Promise<void> =>
    invoke("sftp_mkdir", { sessionId, path }),
  sftpRemove: (sessionId: string, path: string, isDir: boolean): Promise<void> =>
    invoke("sftp_remove", { sessionId, path, isDir }),
  sftpChmod: (sessionId: string, path: string, mode: number): Promise<void> =>
    invoke("sftp_chmod", { sessionId, path, mode }),
  sftpPreviewFile: (sessionId: string, remotePath: string): Promise<RemoteFilePreview> =>
    invoke("sftp_preview_file", { sessionId, remotePath }),
  saveFileDialog: (defaultName?: string): Promise<string | null> =>
    invoke("save_file_dialog", { defaultName: defaultName ?? null }),
  /** Resolve $HOME/Downloads/<filename>, creating the folder if missing
   * and auto-suffixing " (1)", " (2)", … to avoid overwriting. */
  defaultDownloadPath: (remoteFilename: string): Promise<string> =>
    invoke("default_download_path", { remoteFilename }),
  /** Reveal a local file in Finder/Explorer/Files. Used by the SFTP browser
   * after a download so the user has a clickable "Reveal" affordance. */
  revealInFinder: (path: string): Promise<void> =>
    invoke("reveal_in_finder", { path }),

  /** [DIAG-INPUT] fire-and-forget; appends to ~/.tersh/diag.log so the
   *  developer can read the full input pipeline without opening devtools. */
  diagLog: (message: string): void => {
    invoke("diag_log", { message }).catch(() => {});
  },

  // ── vault export/import ──
  exportVault: (passphrase: string): Promise<string> => invoke("export_vault", { passphrase }),
  importVault: (envelope: string, passphrase: string): Promise<void> =>
    invoke("import_vault", { envelope, passphrase }),
  exportVaultToFile: (passphrase: string, path: string): Promise<void> =>
    invoke("export_vault_to_file", { passphrase, path }),
  importVaultFromFile: (path: string, passphrase: string): Promise<void> =>
    invoke("import_vault_from_file", { path, passphrase }),

  // ── ssh key passphrase storage ──
  setKeyPassphrase: (keyId: string, passphrase: string): Promise<void> =>
    invoke("set_key_passphrase", { keyId, passphrase }),
  clearKeyPassphrase: (keyId: string): Promise<void> =>
    invoke("clear_key_passphrase", { keyId }),
  listActiveKeypassKeys: (): Promise<string[]> => invoke("list_active_keypass_keys"),
  setHostPassword: (hostId: string, password: string): Promise<void> =>
    invoke("set_host_password", { hostId, password }),
  clearHostPassword: (hostId: string): Promise<void> =>
    invoke("clear_host_password", { hostId }),
  hasHostPassword: (hostId: string): Promise<boolean> =>
    invoke("has_host_password", { hostId }),

  promptEnhance: (req: PromptEnhanceRequest): Promise<PromptEnhanceResponse> =>
    invoke("prompt_enhance", { req }),
  /// Prompt-enhancer provider API key, persisted encrypted at rest in the
  /// vault (survives launches). Empty string clears it.
  promptEnhancerGetApiKey: (): Promise<string | null> =>
    invoke("prompt_enhancer_get_api_key"),
  promptEnhancerSetApiKey: (apiKey: string): Promise<void> =>
    invoke("prompt_enhancer_set_api_key", { apiKey }),

  // ── project brain (explicit selected-project index + scoped read tools) ──
  brainList: (): Promise<BrainStatus[]> => invoke("brain_list"),
  brainEnableLocal: (projectPath: string, ai?: BrainIndexAiConfig | null): Promise<{ brain_id: string }> =>
    invoke("brain_enable_local", { req: { project_path: projectPath, ai: ai ?? null } }),
  brainEnableRemote: (
    sessionId: string,
    remoteRoot: string | null,
    ai?: BrainIndexAiConfig | null,
    indexId?: string | null,
  ): Promise<{ brain_id: string }> =>
    invoke("brain_enable_remote", {
      req: { session_id: sessionId, remote_root: remoteRoot, ai: ai ?? null, index_id: indexId ?? null },
    }),
  brainDisable: (brainId: string): Promise<void> =>
    invoke("brain_disable", { brainId }),
  // Resolves true if it rebuilt, false if another in-flight refresh on the same
  // brain held the guard and this call was a no-op.
  brainRefresh: (brainId: string, ai?: BrainIndexAiConfig | null): Promise<boolean> =>
    invoke("brain_refresh", { brainId, ai: ai ?? null }),
  /// Auto re-sync a remote project on reconnect — incremental, staleness-gated
  /// backend-side, bound to THIS session. Returns true if it rebuilt.
  brainReconnectResync: (sessionId: string, brainId: string, ai?: BrainIndexAiConfig | null): Promise<boolean> =>
    invoke("brain_reconnect_resync", { sessionId, brainId, ai: ai ?? null }),
  /// Discover candidate project roots on the connected VPS (for the Project
  /// Index dropdown). Auto-detected agent cwd is hoisted to the front.
  brainListRemoteProjects: (sessionId: string): Promise<string[]> =>
    invoke("brain_list_remote_projects", { sessionId }),
  /// Pull project indexes stored inside the given project folders on THIS VPS
  /// (each at <root>/.tersh/) into the registry, so a reconnect shows an
  /// already-indexed project without re-indexing. Returns the number hydrated.
  brainHydrateRemote: (sessionId: string, roots: string[]): Promise<number> =>
    invoke("brain_hydrate_remote", { sessionId, roots }),
  /// Subscribe to live indexing progress. `id` is the pre-allocated index id
  /// passed to brainEnableRemote, or the brain_id on refresh.
  onBrainIndexProgress: (id: string, cb: (p: BrainIndexProgress) => void): Promise<UnlistenFn> =>
    listen<BrainIndexProgress>(`brain://index/${id}/progress`, e => cb(e.payload)),

  // ── file picker ──
  pickFile: (): Promise<string | null> => invoke("pick_file"),
  pickFiles: (): Promise<string[]> => invoke("pick_files"),
  pickFolder: (): Promise<string | null> => invoke("pick_folder"),
  /// Multi-select picker that pre-allocates a transfer_id per file so the
  /// renderer can subscribe to progress events BEFORE kicking off the upload.
  pickUploads: (): Promise<PickedUpload[]> => invoke("pick_uploads"),
  pickUploadFolder: (): Promise<PickedUpload | null> => invoke("pick_upload_folder"),
  /// One native picker that returns any mix of files AND folders, each tagged
  /// with is_dir resolved from real filesystem metadata. Backs the single
  /// Upload button.
  pickUploadsAny: (): Promise<PickedUpload[]> => invoke("pick_uploads_any"),
};

export interface PickedUpload {
  local_path: string;
  transfer_id: string;
  is_dir: boolean;
}

const SENSITIVE = [
  /\.ssh\//, /\/id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$/,
  /\.pem$/, /\.p12$/, /\.pfx$/, /\.keystore$/, /\.kdbx$/, /\.1pux$/,
  /\.aws\/credentials/, /\.kube\/config/, /\/\.env(\.|$)/,
  /wallet\.(dat|json)$/i,
];
export const pathLooksSensitive = (p: string): boolean => SENSITIVE.some(re => re.test(p));

export function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
