import { useEffect, useMemo, useRef, useState } from "react";
import type { MouseEvent } from "react";
import {
  ArrowLeft, ArrowRight, Check, ChevronRight, Copy, Download, Eye, FileText, Folder, FolderClosed, FolderInput,
  FolderPlus, Pencil, RefreshCw, ShieldCheck, Trash2, Upload, X, Loader2,
} from "lucide-react";
import { OsBadge } from "../assets/os-icons";
import { api, pathLooksSensitive } from "../lib/api";
import type { HostRow, RemoteEntry, SftpListing } from "../types";
import { ConnectingView } from "./ConnectingView";

interface Props {
  hosts: HostRow[];
  initialHost?: HostRow | null;
  sessionHost?: HostRow | null;
  sessionId?: string | null;
  initialPath?: string | null;
  onPathChange?: (path: string) => void;
  onSelectHost?: (host: HostRow | null) => void;
  onOpenSession?: (host: HostRow) => void;
}

type Flow =
  | { kind: "picker" }
  | { kind: "connecting"; host: HostRow }
  | { kind: "auth"; host: HostRow; reason: string }
  | { kind: "error"; host: HostRow; message: string }
  | { kind: "ready"; host: HostRow; sessionId: string };

type ActionMode =
  | null
  | { kind: "mkdir"; draft: string }
  | { kind: "rename"; entry: RemoteEntry; draft: string }
  | { kind: "move"; entry: RemoteEntry; draft: string }
  | { kind: "chmod"; entry: RemoteEntry; draft: string }
  | { kind: "delete"; entries: RemoteEntry[] };

type PreviewState =
  | null
  | { kind: "loading"; entry: RemoteEntry }
  | { kind: "image"; entry: RemoteEntry; path: string; dataUrl: string }
  | { kind: "html"; entry: RemoteEntry; path: string; text: string }
  | { kind: "text"; entry: RemoteEntry; path: string; text: string }
  | { kind: "binary"; entry: RemoteEntry; path: string; size: number }
  | { kind: "error"; entry: RemoteEntry; message: string };

interface TransferRow {
  id: string;
  name: string;
  isDir?: boolean;
  direction: "upload" | "download";
  /** Final landing path. For downloads this is the local ~/Downloads/…
   *  destination so the user can click "Reveal" once it lands. */
  destPath?: string;
  bytesDone: number;
  total: number;
  startedAt: number;
  speedBps?: number;
  etaSeconds?: number;
  done: boolean;
  failed?: string;
}

function normalizePath(input: string, cwd = "/"): string {
  const raw = input.trim();
  if (!raw || raw === "~") return "~";
  if (raw.startsWith("~/")) {
    const parts: string[] = [];
    for (const p of raw.slice(2).split("/")) {
      if (!p || p === ".") continue;
      if (p === "..") parts.pop();
      else parts.push(p);
    }
    return parts.length === 0 ? "~" : `~/${parts.join("/")}`;
  }
  const absolute = raw.startsWith("/") ? raw : `${cwd.replace(/\/+$/, "")}/${raw}`;
  const parts: string[] = [];
  for (const p of absolute.split("/")) {
    if (!p || p === ".") continue;
    if (p === "..") parts.pop();
    else parts.push(p);
  }
  return `/${parts.join("/")}`;
}

function basenameOf(path: string): string {
  return path.split("/").filter(Boolean).pop() || "download";
}

function isValidRemoteName(name: string): boolean {
  return !!name && name !== "." && name !== ".." && !name.includes("/") && !name.includes("\0");
}

function extOf(path: string): string {
  const name = basenameOf(path).toLowerCase();
  const dot = name.lastIndexOf(".");
  return dot >= 0 ? name.slice(dot + 1) : "";
}

function imageMimeForExt(ext: string): string | null {
  switch (ext) {
    case "png": return "image/png";
    case "jpg":
    case "jpeg": return "image/jpeg";
    case "gif": return "image/gif";
    case "webp": return "image/webp";
    case "svg": return "image/svg+xml";
    case "bmp": return "image/bmp";
    case "ico": return "image/x-icon";
    default: return null;
  }
}

function isHtmlExt(ext: string): boolean {
  return ext === "html" || ext === "htm" || ext === "xhtml";
}

function isTextExt(ext: string): boolean {
  return new Set([
    "txt", "md", "markdown", "json", "jsonl", "css", "scss", "sass", "less",
    "js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "rb", "rs", "go", "java",
    "kt", "swift", "c", "h", "cpp", "hpp", "cs", "php", "sh", "bash", "zsh",
    "fish", "ps1", "sql", "xml", "yml", "yaml", "toml", "ini", "env", "log",
    "csv", "tsv", "dockerfile", "gitignore",
  ]).has(ext);
}

function dataUrlFromBytes(bytes: Uint8Array, mime: string): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return `data:${mime};base64,${btoa(binary)}`;
}

function looksTextLike(text: string): boolean {
  if (text.includes("\u0000")) return false;
  const controls = text.match(/[\u0001-\u0008\u000b\u000c\u000e-\u001f]/g)?.length ?? 0;
  return controls < Math.max(4, text.length / 100);
}

function redactLocalPaths(message: string): string {
  return message
    .replace(/\/Users\/[^/\s"'`]+(?:\/[^\s"'`]*)?/g, "[local path]")
    .replace(/\/private\/var\/folders\/[^\s"'`]*/g, "[local temp path]")
    .replace(/\/var\/folders\/[^\s"'`]*/g, "[local temp path]");
}

function parentOf(path: string): string {
  if (path === "/" || path === "~") return path;
  const parts = path.split("/").filter(Boolean);
  parts.pop();
  if (path.startsWith("/")) return parts.length === 0 ? "/" : `/${parts.join("/")}`;
  return parts.length === 0 ? "~" : `~/${parts.join("/")}`;
}

function joinPath(dir: string, name: string): string {
  const base = dir === "~" ? "~" : normalizePath(dir);
  return normalizePath(`${base}/${name}`);
}

function isDescendantPath(path: string, parent: string): boolean {
  const normalizedPath = normalizePath(path);
  const normalizedParent = normalizePath(parent);
  if (normalizedPath === normalizedParent) return false;
  return normalizedPath.startsWith(`${normalizedParent.replace(/\/+$/, "")}/`);
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtRate(bytesPerSecond?: number): string | null {
  if (!bytesPerSecond || !Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return null;
  return `${fmtBytes(bytesPerSecond)}/s`;
}

function fmtDuration(seconds?: number): string | null {
  if (!seconds || !Number.isFinite(seconds) || seconds <= 0) return null;
  const rounded = Math.max(1, Math.round(seconds));
  if (rounded < 60) return `${rounded}s`;
  const minutes = Math.floor(rounded / 60);
  const secs = rounded % 60;
  if (minutes < 60) return secs > 0 ? `${minutes}m ${secs}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const mins = minutes % 60;
  return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
}

function isAuthError(message: string): boolean {
  const m = message.toLowerCase();
  return m.includes("password required") || m.includes("passphrase") || m.includes("auth");
}

export function SftpPage({
  hosts,
  initialHost,
  sessionHost,
  sessionId,
  initialPath,
  onPathChange,
  onSelectHost,
  onOpenSession,
}: Props) {
  const managedSession = sessionHost && sessionId ? { host: sessionHost, sessionId } : null;
  const managedInitialPath = initialPath && initialPath.trim() ? initialPath : "~";
  const [flow, setFlow] = useState<Flow>(() => managedSession
    ? { kind: "ready", host: managedSession.host, sessionId: managedSession.sessionId }
    : initialHost
      ? { kind: "connecting", host: initialHost }
      : { kind: "picker" });
  const [listing, setListing] = useState<SftpListing>({ cwd: "~", entries: [], truncated: false });
  const [loadingPath, setLoadingPath] = useState<string | null>(null);
  const [loadingMessage, setLoadingMessage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pathDraft, setPathDraft] = useState("~");
  const [backStack, setBackStack] = useState<string[]>([]);
  const [forwardStack, setForwardStack] = useState<string[]>([]);
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(() => new Set());
  const selectionAnchor = useRef<string | null>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; entry: RemoteEntry } | null>(null);
  const [pathEditing, setPathEditing] = useState(false);
  const [actionMode, setActionMode] = useState<ActionMode>(null);
  const [preview, setPreview] = useState<PreviewState>(null);
  const [transfers, setTransfers] = useState<TransferRow[]>([]);
  const transferUnlisten = useRef(new Map<string, () => void>());
  const transferDismissTimers = useRef(new Map<string, number>());
  const pathInputRef = useRef<HTMLInputElement>(null);
  const actionInputRef = useRef<HTMLInputElement>(null);
  const requestId = useRef(0);
  const actionId = useRef(0);
  const sessionRef = useRef<string | null>(null);
  const disposed = useRef(false);
  const navigationBusy = useRef(false);

  function clearSelection() {
    setSelectedPaths(new Set());
    selectionAnchor.current = null;
  }

  function selectOnly(entry: RemoteEntry) {
    setSelectedPaths(new Set([entry.path]));
    selectionAnchor.current = entry.path;
  }

  function selectEntry(entry: RemoteEntry, event: MouseEvent<HTMLElement>) {
    if (event.shiftKey && selectionAnchor.current) {
      const anchorIndex = listing.entries.findIndex(item => item.path === selectionAnchor.current);
      const entryIndex = listing.entries.findIndex(item => item.path === entry.path);
      if (anchorIndex >= 0 && entryIndex >= 0) {
        const [start, end] = anchorIndex < entryIndex
          ? [anchorIndex, entryIndex]
          : [entryIndex, anchorIndex];
        const range = listing.entries.slice(start, end + 1);
        setSelectedPaths(new Set(range.map(item => item.path)));
        return;
      }
    }

    if (event.metaKey || event.ctrlKey) {
      setSelectedPaths(prev => {
        const next = new Set(prev);
        if (next.has(entry.path)) next.delete(entry.path);
        else next.add(entry.path);
        return next;
      });
      selectionAnchor.current = entry.path;
      return;
    }

    selectOnly(entry);
  }

  function selectedEntriesFor(entry: RemoteEntry) {
    if (!selectedPaths.has(entry.path)) return [entry];
    const entries = listing.entries.filter(item => selectedPaths.has(item.path));
    if (entries.length === 0) return [entry];
    return entries.filter(item =>
      !entries.some(parent => parent.is_dir && isDescendantPath(item.path, parent.path)),
    );
  }

  useEffect(() => {
    if (managedSession) return;
    if (onOpenSession) return;
    if (initialHost) {
      void connect(initialHost);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialHost?.id, onOpenSession]);

  useEffect(() => {
    return () => {
      disposed.current = true;
      requestId.current += 1;
      if (!managedSession && sessionRef.current) api.disconnect(sessionRef.current).catch(() => {});
      // Drop all in-flight progress listeners. We don't cancel the underlying
      // transfers — they finish in the background; we just stop spending UI
      // cycles updating a state that no longer exists.
      for (const off of transferUnlisten.current.values()) off();
      transferUnlisten.current.clear();
      for (const t of transferDismissTimers.current.values()) window.clearTimeout(t);
      transferDismissTimers.current.clear();
    };
  }, [managedSession?.sessionId]);

  // Autofocus the action input when a new action mode opens.
  useEffect(() => {
    if (actionMode && actionMode.kind !== "delete") {
      requestAnimationFrame(() => actionInputRef.current?.select());
    }
  }, [actionMode?.kind, (actionMode as { entry?: RemoteEntry } | null)?.entry?.path]);

  useEffect(() => {
    if (!contextMenu) return;
    const close = () => setContextMenu(null);
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("click", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [contextMenu]);

  const closeSftpSession = () => {
    if (!managedSession && sessionRef.current) {
      api.disconnect(sessionRef.current).catch(() => {});
      sessionRef.current = null;
    }
  };

  const changeHost = () => {
    if (managedSession) return;
    requestId.current += 1;
    actionId.current += 1;
    navigationBusy.current = false;
    closeSftpSession();
    onSelectHost?.(null);
    setFlow({ kind: "picker" });
    setListing({ cwd: "~", entries: [], truncated: false });
    setPathDraft("~");
    setLoadingPath(null);
    setLoadingMessage("");
    setError(null);
    clearSelection();
    setBackStack([]);
    setForwardStack([]);
    setActionMode(null);
  };

  const connect = async (host: HostRow, secret: string | null = null, remember = false) => {
    const id = ++requestId.current;
    actionId.current += 1;
    navigationBusy.current = false;
    if (sessionRef.current) {
      api.disconnect(sessionRef.current).catch(() => {});
      sessionRef.current = null;
    }
    onSelectHost?.(host);
    setFlow({ kind: "connecting", host });
    setListing({ cwd: "~", entries: [], truncated: false });
    clearSelection();
    setBackStack([]);
    setForwardStack([]);
    setError(null);
    try {
      const resp = await api.connect({
        host_id: host.id,
        auth_secret: secret,
        cols: 100,
        rows: 30,
        remember_key_passphrase: remember,
      });
      if (disposed.current || id !== requestId.current) {
        api.disconnect(resp.session_id).catch(() => {});
        return;
      }
      sessionRef.current = resp.session_id;
      setFlow({ kind: "ready", host, sessionId: resp.session_id });
      await navigate(resp.session_id, "~", { pushHistory: false, message: "Opening remote directory" });
    } catch (e) {
      if (disposed.current || id !== requestId.current) return;
      const message = String(e);
      if (isAuthError(message)) {
        setFlow({
          kind: "auth",
          host,
          reason: host.auth_kind === "password" ? "Enter password" : "Enter key passphrase",
        });
      } else {
        setFlow({ kind: "error", host, message });
      }
    }
  };

  const navigate = async (
    sid: string,
    rawPath: string,
    opts: { pushHistory?: boolean; message?: string } = {},
  ): Promise<boolean> => {
    if (navigationBusy.current) return false;
    const current = listing.cwd;
    const target = rawPath === "~" ? "~" : normalizePath(rawPath, current === "~" ? "/" : current);
    const id = ++requestId.current;
    navigationBusy.current = true;
    setLoadingPath(target);
    setLoadingMessage(opts.message ?? `Opening ${target}`);
    setError(null);
    clearSelection();
    setActionMode(null);
    try {
      const next = await api.sftpListRemote(sid, target);
      if (disposed.current || id !== requestId.current) return false;
      setListing(next);
      setPathDraft(next.cwd);
      onPathChange?.(next.cwd);
      if (opts.pushHistory && current && current !== next.cwd) {
        setBackStack(prev => [...prev, current]);
        setForwardStack([]);
      }
      return true;
    } catch (e) {
      if (disposed.current || id !== requestId.current) return false;
      setError(String(e));
      return false;
    } finally {
      if (!disposed.current && id === requestId.current) {
        navigationBusy.current = false;
        setLoadingPath(null);
        setLoadingMessage("");
      }
    }
  };

  useEffect(() => {
    if (!managedSession) return;
    disposed.current = false;
    requestId.current += 1;
    actionId.current += 1;
    navigationBusy.current = false;
    sessionRef.current = managedSession.sessionId;
    onSelectHost?.(managedSession.host);
    setFlow({ kind: "ready", host: managedSession.host, sessionId: managedSession.sessionId });
    setListing({ cwd: managedInitialPath, entries: [], truncated: false });
    setPathDraft(managedInitialPath);
    clearSelection();
    setBackStack([]);
    setForwardStack([]);
    setError(null);
    void navigate(managedSession.sessionId, managedInitialPath, {
      pushHistory: false,
      message: "Opening remote directory",
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [managedSession?.sessionId, managedSession?.host.id]);

  const ready = flow.kind === "ready" ? flow : null;
  const currentHost = flow.kind === "picker" ? null : flow.host;
  const crumbs = useMemo(() => {
    if (listing.cwd === "~") return [];
    return normalizePath(listing.cwd).split("/").filter(Boolean);
  }, [listing.cwd]);

  // ── transfer plumbing ────────────────────────────────────────────────────
  // Mirrors the TerminalView pattern: subscribe to the per-transfer progress
  // channel BEFORE kicking off the backend call so the first packet can't
  // race us. Auto-dismiss on completion (2.5s) or failure (4.5s).

  const subscribeTransfer = (id: string) => {
    api.onTransferProgress(id, p => {
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === id
          ? (() => {
              const elapsed = Math.max(0.5, (Date.now() - t.startedAt) / 1000);
              const speedBps = p.bytes_done > 0 ? p.bytes_done / elapsed : undefined;
              const etaSeconds = speedBps && p.total > p.bytes_done
                ? (p.total - p.bytes_done) / speedBps
                : undefined;
              return {
                ...t,
                bytesDone: p.bytes_done,
                total: p.total,
                speedBps,
                etaSeconds,
                done: p.done,
              };
            })()
          : t,
      ));
    }).then(off => {
      if (disposed.current) { off(); return; }
      transferUnlisten.current.set(id, off);
    }).catch(() => {});
  };

  const scheduleTransferDismiss = (id: string, after: number) => {
    if (disposed.current) return;
    const prior = transferDismissTimers.current.get(id);
    if (prior !== undefined) window.clearTimeout(prior);
    const timer = window.setTimeout(() => {
      if (disposed.current) return;
      setTransfers(prev => prev.filter(t => t.id !== id));
      const off = transferUnlisten.current.get(id);
      if (off) off();
      transferUnlisten.current.delete(id);
      transferDismissTimers.current.delete(id);
    }, after);
    transferDismissTimers.current.set(id, timer);
  };

  const cancelTransfer = (id: string) => {
    if (disposed.current) return;
    api.sftpCancelTransfer(id).catch(() => {});
    setTransfers(prev => prev.map(t =>
      t.id === id && !t.done ? { ...t, done: true, failed: "cancelled" } : t,
    ));
    scheduleTransferDismiss(id, 1500);
  };

  const dismissTransferRow = (id: string) => {
    setTransfers(prev => prev.filter(t => t.id !== id));
    const off = transferUnlisten.current.get(id);
    if (off) off();
    transferUnlisten.current.delete(id);
    const timer = transferDismissTimers.current.get(id);
    if (timer !== undefined) window.clearTimeout(timer);
    transferDismissTimers.current.delete(id);
  };

  const kickoffUpload = async (sid: string, localPath: string, transferId: string, sensitive: boolean) => {
    if (sensitive) {
      setError("Upload blocked: sensitive file.");
      scheduleTransferDismiss(transferId, 1500);
      return;
    }
    const name = basenameOf(localPath);
    setTransfers(prev => [...prev, {
      id: transferId, name, direction: "upload", bytesDone: 0, total: 0, startedAt: Date.now(), done: false,
    }]);
    subscribeTransfer(transferId);
    try {
      const res = await api.sftpUploadTo(
        sid,
        localPath,
        joinPath(listing.cwd, name),
        transferId,
      );
      if (disposed.current) return;
      // Backend emits a final done event, but if it fired before our listener
      // attached we'd be stuck — make sure the row marks done either way.
      setTransfers(prev => prev.map(t =>
        t.id === transferId ? { ...t, done: true, destPath: res.remote_path } : t,
      ));
      scheduleTransferDismiss(transferId, 2500);
      if (ready) await navigate(sid, listing.cwd, { pushHistory: false, message: "Refreshing" });
    } catch (e) {
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === transferId ? { ...t, done: true, failed: redactLocalPaths(String(e)) } : t,
      ));
      scheduleTransferDismiss(transferId, 4500);
    }
  };

  const kickoffFolderUpload = async (sid: string, localPath: string, transferId: string) => {
    const name = basenameOf(localPath);
    setTransfers(prev => [...prev, {
      id: transferId, name, isDir: true, direction: "upload", bytesDone: 0, total: 0, startedAt: Date.now(), done: false,
    }]);
    subscribeTransfer(transferId);
    try {
      const res = await api.sftpUploadFolderTo(
        sid,
        localPath,
        listing.cwd,
        transferId,
      );
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === transferId ? { ...t, done: true, destPath: res.remote_path } : t,
      ));
      scheduleTransferDismiss(transferId, 2500);
      if (ready) await navigate(sid, listing.cwd, { pushHistory: false, message: "Refreshing" });
    } catch (e) {
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === transferId ? { ...t, done: true, failed: redactLocalPaths(String(e)) } : t,
      ));
      scheduleTransferDismiss(transferId, 4500);
    }
  };

  const kickoffDownload = async (sid: string, remotePath: string) => {
    const name = basenameOf(remotePath);
    let dest: string;
    try {
      dest = await api.defaultDownloadPath(name);
    } catch (e) {
      setError(String(e));
      return;
    }
    if (disposed.current) return;
    const transferId = crypto.randomUUID();
    setTransfers(prev => [...prev, {
      id: transferId, name, direction: "download", destPath: dest,
      bytesDone: 0, total: 0, startedAt: Date.now(), done: false,
    }]);
    subscribeTransfer(transferId);
    try {
      await api.sftpDownload(sid, remotePath, dest, transferId);
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === transferId ? { ...t, done: true } : t,
      ));
      // Don't auto-dismiss downloads as aggressively — the user needs a
      // chance to click Reveal.
      scheduleTransferDismiss(transferId, 6000);
    } catch (e) {
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === transferId ? { ...t, done: true, failed: redactLocalPaths(String(e)) } : t,
      ));
      scheduleTransferDismiss(transferId, 4500);
    }
  };

  // ── inline action commits ────────────────────────────────────────────────

  const goBack = async () => {
    if (!ready || backStack.length === 0) return;
    const target = backStack[backStack.length - 1]!;
    const current = listing.cwd;
    const ok = await navigate(ready.sessionId, target, { pushHistory: false, message: "Going back" });
    if (ok) {
      setBackStack(prev => prev.slice(0, -1));
      setForwardStack(prev => [current, ...prev]);
    }
  };

  const goForward = async () => {
    if (!ready || forwardStack.length === 0) return;
    const target = forwardStack[0]!;
    const current = listing.cwd;
    const ok = await navigate(ready.sessionId, target, { pushHistory: false, message: "Going forward" });
    if (ok) {
      setForwardStack(prev => prev.slice(1));
      setBackStack(prev => [...prev, current]);
    }
  };

  const refresh = async () => {
    if (!ready) return;
    await navigate(ready.sessionId, listing.cwd, { pushHistory: false, message: "Refreshing" });
  };

  const openEntry = async (entry: RemoteEntry) => {
    if (!ready || loadingPath || navigationBusy.current || !entry.is_dir) return;
    await navigate(ready.sessionId, entry.path, { pushHistory: true });
  };

  const startMkdir = () => setActionMode({ kind: "mkdir", draft: "" });
  const startRename = (entry: RemoteEntry) => setActionMode({ kind: "rename", entry, draft: entry.name });
  const startMove = (entry: RemoteEntry) => setActionMode({ kind: "move", entry, draft: parentOf(entry.path) });
  const startChmod = (entry: RemoteEntry) => setActionMode({ kind: "chmod", entry, draft: "" });
  const startDelete = (entry: RemoteEntry) => setActionMode({ kind: "delete", entries: selectedEntriesFor(entry) });
  const cancelAction = () => setActionMode(null);

  const commitMkdir = async () => {
    if (!ready || !actionMode || actionMode.kind !== "mkdir") return;
    const name = actionMode.draft.trim();
    if (!name) return;
    if (!isValidRemoteName(name)) {
      setError("Folder name cannot contain slashes or path traversal.");
      return;
    }
    const id = ++actionId.current;
    try {
      await api.sftpMkdir(ready.sessionId, joinPath(listing.cwd, name));
      if (disposed.current || id !== actionId.current) return;
      setActionMode(null);
      await refresh();
    } catch (e) {
      if (!disposed.current && id === actionId.current) setError(String(e));
    }
  };

  const commitRename = async () => {
    if (!ready || !actionMode || actionMode.kind !== "rename") return;
    const newName = actionMode.draft.trim();
    if (!newName || newName === actionMode.entry.name) {
      setActionMode(null);
      return;
    }
    if (!isValidRemoteName(newName)) {
      setError("Name cannot contain slashes or path traversal.");
      return;
    }
    const id = ++actionId.current;
    const oldPath = actionMode.entry.path;
    const newPath = joinPath(parentOf(oldPath), newName);
    try {
      await api.sftpRename(ready.sessionId, oldPath, newPath);
      if (disposed.current || id !== actionId.current) return;
      setActionMode(null);
      clearSelection();
      await refresh();
    } catch (e) {
      if (!disposed.current && id === actionId.current) setError(String(e));
    }
  };

  const commitMove = async () => {
    if (!ready || !actionMode || actionMode.kind !== "move") return;
    const destDir = normalizePath(actionMode.draft.trim() || "/", listing.cwd);
    const id = ++actionId.current;
    const oldPath = actionMode.entry.path;
    const newPath = joinPath(destDir, actionMode.entry.name);
    if (actionMode.entry.is_dir && (destDir === oldPath || isDescendantPath(destDir, oldPath))) {
      setError("Folder cannot be moved inside itself.");
      return;
    }
    if (newPath === oldPath) {
      setActionMode(null);
      return;
    }
    try {
      await api.sftpRename(ready.sessionId, oldPath, newPath);
      if (disposed.current || id !== actionId.current) return;
      setActionMode(null);
      clearSelection();
      await refresh();
    } catch (e) {
      if (!disposed.current && id === actionId.current) setError(String(e));
    }
  };

  const commitChmod = async () => {
    if (!ready || !actionMode || actionMode.kind !== "chmod") return;
    const draft = actionMode.draft.trim();
    if (!/^[0-7]{3,4}$/.test(draft)) {
      setError("Permissions must be an octal mode like 644 or 755.");
      return;
    }
    const id = ++actionId.current;
    try {
      await api.sftpChmod(ready.sessionId, actionMode.entry.path, parseInt(draft, 8));
      if (disposed.current || id !== actionId.current) return;
      setActionMode(null);
      await refresh();
    } catch (e) {
      if (!disposed.current && id === actionId.current) setError(String(e));
    }
  };

  const commitDelete = async () => {
    if (!ready || !actionMode || actionMode.kind !== "delete") return;
    const id = ++actionId.current;
    const targets = actionMode.entries;
    try {
      for (const target of targets) {
        await api.sftpRemove(ready.sessionId, target.path, target.is_dir);
      }
      if (disposed.current || id !== actionId.current) return;
      setActionMode(null);
      clearSelection();
      await refresh();
    } catch (e) {
      if (!disposed.current && id === actionId.current) setError(String(e));
    }
  };

  // One picker for both files and folders. Each picked item carries is_dir
  // (resolved from real filesystem metadata in the backend), so a mixed
  // selection routes per item: folders recurse via sftpUploadFolderTo, files
  // go through sftpUploadTo.
  const uploadFilesOrFolders = async () => {
    if (!ready) return;
    let picked: { local_path: string; transfer_id: string; is_dir: boolean }[] = [];
    try {
      picked = await api.pickUploadsAny();
    } catch (e) {
      setError(String(e));
      return;
    }
    if (disposed.current) return;
    for (const item of picked) {
      if (disposed.current) return;
      if (item.is_dir) {
        void kickoffFolderUpload(ready.sessionId, item.local_path, item.transfer_id);
      } else {
        const sensitive = pathLooksSensitive(item.local_path);
        void kickoffUpload(ready.sessionId, item.local_path, item.transfer_id, sensitive);
      }
    }
  };

  const copyRemotePath = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
    } catch (e) {
      setError(String(e));
    }
  };

  const startPreview = async (entry: RemoteEntry) => {
    if (!ready || entry.is_dir) return;
    setPreview({ kind: "loading", entry });
    try {
      const res = await api.sftpPreviewFile(ready.sessionId, entry.path);
      const bytes = Uint8Array.from(res.bytes);
      const ext = extOf(entry.name || res.path);
      const imageMime = imageMimeForExt(ext);
      if (imageMime) {
        setPreview({
          kind: "image",
          entry,
          path: res.path,
          dataUrl: dataUrlFromBytes(bytes, imageMime),
        });
        return;
      }
      const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
      if (isHtmlExt(ext)) {
        setPreview({ kind: "html", entry, path: res.path, text });
        return;
      }
      if (isTextExt(ext) || looksTextLike(text)) {
        setPreview({ kind: "text", entry, path: res.path, text });
        return;
      }
      setPreview({ kind: "binary", entry, path: res.path, size: bytes.length });
    } catch (e) {
      setPreview({ kind: "error", entry, message: String(e) });
    }
  };

  if (flow.kind === "picker") {
    return (
      <div className="sftp-page">
        <HostPicker
          hosts={hosts}
          onConnect={onOpenSession ?? connect}
        />
      </div>
    );
  }

  if (flow.kind === "connecting") {
    return (
      <div className="sftp-page">
        <ConnectingView host={flow.host} onCancel={changeHost} />
      </div>
    );
  }

  if (flow.kind === "auth") {
    return (
      <div className="sftp-page">
        <ConnectingView
          host={flow.host}
          authPrompt={flow.reason}
          onConnect={(secret, remember) => connect(flow.host, secret, remember)}
          onCancel={changeHost}
        />
      </div>
    );
  }

  if (flow.kind === "error") {
    return (
      <div className="sftp-page">
        <ConnectingView
          host={flow.host}
          error={flow.message}
          onCancel={changeHost}
          onRetry={() => connect(flow.host)}
        />
      </div>
    );
  }

  return (
    <div className="sftp-page">
      <header className="sftp-explorer-head">
        <div className="sftp-host-id">
          <OsBadge os={currentHost?.os ?? "linux"} size={30} />
          <div>
            <strong>{currentHost?.label}</strong>
            <span>{currentHost?.username}@{currentHost?.hostname}:{currentHost?.port}</span>
          </div>
        </div>
        {!managedSession && <button className="toolbar-btn" onClick={changeHost}>Change Host</button>}
      </header>

      <div className="sftp-explorer-toolbar">
        <button className="icon-tool" title="Back" aria-label="Back" disabled={backStack.length === 0 || !!loadingPath} onClick={goBack}>
          <ArrowLeft size={15} />
        </button>
        <button className="icon-tool" title="Forward" aria-label="Forward" disabled={forwardStack.length === 0 || !!loadingPath} onClick={goForward}>
          <ArrowRight size={15} />
        </button>
        <button className="icon-tool" title="Refresh" aria-label="Refresh" disabled={!!loadingPath} onClick={refresh}>
          <RefreshCw size={15} className={loadingPath ? "spin" : ""} />
        </button>
        {pathEditing ? (
          <form
            className="sftp-pathbar editing"
            onSubmit={(e) => {
              e.preventDefault();
              setPathEditing(false);
              if (ready && !loadingPath && pathDraft.trim() && pathDraft !== listing.cwd) {
                void navigate(ready.sessionId, pathDraft, { pushHistory: true, message: "Opening path" });
              }
            }}
          >
            <input
              ref={pathInputRef}
              aria-label="Remote path"
              value={pathDraft}
              onChange={e => setPathDraft(e.target.value)}
              onBlur={() => { setPathEditing(false); setPathDraft(listing.cwd); }}
              onKeyDown={(e) => { if (e.key === "Escape") { setPathEditing(false); setPathDraft(listing.cwd); } }}
              disabled={!!loadingPath}
              spellCheck={false}
              autoFocus
            />
          </form>
        ) : (
          <button
            type="button"
            className="sftp-pathbar"
            aria-label="Edit path"
            onClick={() => {
              setPathDraft(listing.cwd);
              setPathEditing(true);
              requestAnimationFrame(() => pathInputRef.current?.select());
            }}
          >
            <span className="sftp-breadcrumbs">
              <span
                className="crumb root"
                role="button"
                tabIndex={0}
                onClick={(e) => { e.stopPropagation(); if (ready) void navigate(ready.sessionId, "/", { pushHistory: true }); }}
                onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.stopPropagation(); if (ready) void navigate(ready.sessionId, "/", { pushHistory: true }); } }}
              >/</span>
              {crumbs.map((c, i) => (
                <span key={`${c}-${i}`} className="crumb-wrap">
                  <ChevronRight size={11} aria-hidden />
                  <span
                    className="crumb"
                    role="button"
                    tabIndex={0}
                    onClick={(e) => { e.stopPropagation(); if (ready) void navigate(ready.sessionId, `/${crumbs.slice(0, i + 1).join("/")}`, { pushHistory: true }); }}
                    onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.stopPropagation(); if (ready) void navigate(ready.sessionId, `/${crumbs.slice(0, i + 1).join("/")}`, { pushHistory: true }); } }}
                  >{c}</span>
                </span>
              ))}
            </span>
          </button>
        )}
        <button className="toolbar-btn" disabled={!!loadingPath || !!actionMode} onClick={startMkdir}><FolderPlus size={14} /> New Folder</button>
        <button className="toolbar-btn" disabled={!!loadingPath || !!actionMode} onClick={uploadFilesOrFolders}><Upload size={14} /> Upload</button>
      </div>

      {actionMode && (
        <div className="sftp-action-strip">
          {actionMode.kind === "mkdir" && (
            <form onSubmit={(e) => { e.preventDefault(); void commitMkdir(); }}>
              <FolderPlus size={14} aria-hidden />
              <span className="sftp-action-label">New folder in <code>{listing.cwd}</code></span>
              <input
                ref={actionInputRef}
                aria-label="Folder name"
                value={actionMode.draft}
                onChange={e => setActionMode({ kind: "mkdir", draft: e.target.value })}
                onKeyDown={e => { if (e.key === "Escape") cancelAction(); }}
                placeholder="folder-name"
                autoFocus
                spellCheck={false}
              />
              <button type="submit" className="toolbar-btn primary-ghost" disabled={!actionMode.draft.trim()}><Check size={14} /> Create</button>
              <button type="button" className="toolbar-btn" onClick={cancelAction}><X size={14} /> Cancel</button>
            </form>
          )}
          {actionMode.kind === "rename" && (
            <form onSubmit={(e) => { e.preventDefault(); void commitRename(); }}>
              <Pencil size={14} aria-hidden />
              <span className="sftp-action-label">Rename <code>{actionMode.entry.name}</code></span>
              <input
                ref={actionInputRef}
                aria-label="New name"
                value={actionMode.draft}
                onChange={e => setActionMode({ kind: "rename", entry: actionMode.entry, draft: e.target.value })}
                onKeyDown={e => { if (e.key === "Escape") cancelAction(); }}
                autoFocus
                spellCheck={false}
              />
              <button type="submit" className="toolbar-btn primary-ghost" disabled={!actionMode.draft.trim() || actionMode.draft.trim() === actionMode.entry.name}><Check size={14} /> Rename</button>
              <button type="button" className="toolbar-btn" onClick={cancelAction}><X size={14} /> Cancel</button>
            </form>
          )}
          {actionMode.kind === "move" && (
            <form onSubmit={(e) => { e.preventDefault(); void commitMove(); }}>
              <FolderInput size={14} aria-hidden />
              <span className="sftp-action-label">Move <code>{actionMode.entry.name}</code> to</span>
              <input
                ref={actionInputRef}
                aria-label="Destination directory"
                value={actionMode.draft}
                onChange={e => setActionMode({ kind: "move", entry: actionMode.entry, draft: e.target.value })}
                onKeyDown={e => { if (e.key === "Escape") cancelAction(); }}
                autoFocus
                spellCheck={false}
                placeholder="/destination/dir"
              />
              <button type="submit" className="toolbar-btn primary-ghost" disabled={!actionMode.draft.trim()}><Check size={14} /> Move</button>
              <button type="button" className="toolbar-btn" onClick={cancelAction}><X size={14} /> Cancel</button>
            </form>
          )}
          {actionMode.kind === "chmod" && (
            <form onSubmit={(e) => { e.preventDefault(); void commitChmod(); }}>
              <ShieldCheck size={14} aria-hidden />
              <span className="sftp-action-label">Permissions for <code>{actionMode.entry.name}</code></span>
              <input
                ref={actionInputRef}
                aria-label="Octal permissions"
                value={actionMode.draft}
                onChange={e => setActionMode({ kind: "chmod", entry: actionMode.entry, draft: e.target.value })}
                onKeyDown={e => { if (e.key === "Escape") cancelAction(); }}
                autoFocus
                spellCheck={false}
                placeholder="644"
              />
              <button type="submit" className="toolbar-btn primary-ghost" disabled={!/^[0-7]{3,4}$/.test(actionMode.draft.trim())}><Check size={14} /> Apply</button>
              <button type="button" className="toolbar-btn" onClick={cancelAction}><X size={14} /> Cancel</button>
            </form>
          )}
          {actionMode.kind === "delete" && (
            <div className="sftp-action-row">
              <Trash2 size={14} aria-hidden />
              <span className="sftp-action-label">
                {actionMode.entries.length === 1 ? (
                  <>
                    Delete {actionMode.entries[0]!.is_dir ? "folder" : "file"} <code>{actionMode.entries[0]!.name}</code>?
                  </>
                ) : (
                  <>Delete {actionMode.entries.length} selected items?</>
                )}
                {actionMode.entries.some(entry => entry.is_dir) && <span className="muted"> Folder contents will be removed.</span>}
              </span>
              <button type="button" className="toolbar-btn danger" onClick={() => void commitDelete()}><Check size={14} /> Delete</button>
              <button type="button" className="toolbar-btn" onClick={cancelAction}><X size={14} /> Cancel</button>
            </div>
          )}
        </div>
      )}

      {transfers.length > 0 && (
        <div className="sftp-transfer-queue" role="status" aria-live="polite">
          {transfers.map(t => {
            const pct = t.total > 0 ? Math.min(100, Math.round((t.bytesDone / t.total) * 100)) : (t.done ? 100 : 0);
            const cls = "sftp-transfer" + (t.failed ? " failed" : t.done ? " done" : "");
            const rate = fmtRate(t.speedBps);
            const eta = fmtDuration(t.etaSeconds);
            const progressText = t.total > 0
              ? [
                  `${fmtBytes(t.bytesDone)} / ${fmtBytes(t.total)}`,
                  rate,
                  eta ? `ETA ${eta}` : null,
                ].filter(Boolean).join("  ·  ")
              : "Starting";
            const action = t.done && !t.failed && t.direction === "download" && t.destPath ? (
              <button
                type="button"
                className="icon-tool"
                title="Reveal in Finder"
                aria-label="Reveal in Finder"
                onClick={() => { if (t.destPath) api.revealInFinder(t.destPath).catch(err => setError(String(err))); }}
              >
                <Eye size={12} />
              </button>
            ) : (
              <button
                type="button"
                className="icon-tool"
                title={t.done ? "Dismiss" : "Cancel"}
                aria-label={t.done ? "Dismiss" : "Cancel"}
                onClick={() => t.done ? dismissTransferRow(t.id) : cancelTransfer(t.id)}
              >
                <X size={11} />
              </button>
            );
            return (
              <div key={t.id} className={cls}>
                <div className="sftp-transfer-head">
                  <span className="sftp-transfer-dir" title={t.direction}>
                    {t.direction === "upload" ? <Upload size={12} /> : <Download size={12} />}
                  </span>
                  <span className="sftp-transfer-name" title={t.destPath ?? t.name}>{t.name}</span>
                  {!t.failed && !t.done && t.total > 0 && (
                    <span className="sftp-transfer-pct">{pct}%</span>
                  )}
                  {action}
                </div>
                <div className="sftp-transfer-bar" aria-hidden>
                  <div className="sftp-transfer-fill" style={{ width: `${t.failed ? 100 : pct}%` }} />
                </div>
                <span className="sftp-transfer-bytes">
                  {t.failed
                    ? t.failed
                    : t.done
                      ? (t.destPath
                        ? <>{t.direction === "download" ? "saved to" : "uploaded to"} <code>{t.destPath}</code></>
                        : "done")
                      : progressText}
                </span>
              </div>
            );
          })}
        </div>
      )}

      {loadingPath && (
        <div className="sftp-loading-line">
          <Loader2 size={12} strokeWidth={2.25} className="spin" aria-hidden />
          <span>{loadingMessage}</span>
        </div>
      )}
      {error && (
        <div className="sftp-error-line">
          <span>{error}</span>
          <button onClick={() => setError(null)}>Dismiss</button>
          {ready && <button onClick={refresh}>Retry</button>}
        </div>
      )}
      {listing.truncated && (
        <div className="sftp-loading-line muted">
          Directory is huge — showing the first {listing.entries.length.toLocaleString()} entries.
        </div>
      )}
      <div className="sftp-file-table">
        <div className="sftp-file-head">
          <span>Name</span>
          <span>Size</span>
          <span>Modified</span>
        </div>
        {listing.entries.length === 0 && !loadingPath && !error && (
          <div className="sftp-empty-state">
            <FolderClosed size={30} strokeWidth={1.5} aria-hidden />
            <span>This folder is empty</span>
          </div>
        )}
        {loadingPath && listing.entries.length === 0 && (
          <div className="sftp-skeleton-list">
            {Array.from({ length: 8 }).map((_, i) => <div key={i} />)}
          </div>
        )}
        {listing.entries.map(entry => (
          <button
            key={entry.path}
            type="button"
            className={"sftp-file-row" + (entry.is_dir ? " is-dir" : "") + (selectedPaths.has(entry.path) ? " selected" : "")}
            aria-selected={selectedPaths.has(entry.path)}
            onClick={(event) => {
              if (entry.is_dir && !event.metaKey && !event.ctrlKey && !event.shiftKey) {
                void openEntry(entry);
                return;
              }
              selectEntry(entry, event);
            }}
            onDoubleClick={() => entry.is_dir ? openEntry(entry) : void startPreview(entry)}
            onContextMenu={(event) => {
              event.preventDefault();
              event.stopPropagation();
              if (!selectedPaths.has(entry.path)) selectOnly(entry);
              setContextMenu({ x: event.clientX, y: event.clientY, entry });
            }}
            disabled={!!loadingPath}
            title={entry.path}
          >
            <span className="sftp-file-name">
              {entry.is_dir ? <FolderClosed size={15} /> : <FileText size={15} />}
              {loadingPath === entry.path && <RefreshCw size={12} className="spin" />}
              <span>{entry.name}</span>
            </span>
            <span>{entry.is_dir ? "—" : fmtBytes(entry.size)}</span>
            <span>{entry.modified
              ? new Date(entry.modified * 1000).toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })
              : "—"}</span>
          </button>
        ))}
      </div>

      {contextMenu && (
        <div
          className="host-context-menu sftp-file-menu"
          style={{
            left: Math.min(contextMenu.x, window.innerWidth - 250),
            top: Math.min(contextMenu.y, window.innerHeight - 320),
          }}
          onClick={(event) => event.stopPropagation()}
          onContextMenu={(event) => event.preventDefault()}
        >
          {contextMenu.entry.is_dir && (
            <button
              type="button"
              className="host-context-item"
              onClick={() => {
                const entry = contextMenu.entry;
                setContextMenu(null);
                void openEntry(entry);
              }}
            >
              <FolderClosed size={14} /> Open
            </button>
          )}
          {!contextMenu.entry.is_dir && ready && (
            <>
              <button
                type="button"
                className="host-context-item"
                onClick={() => {
                  const entry = contextMenu.entry;
                  setContextMenu(null);
                  void startPreview(entry);
                }}
              >
                <Eye size={14} /> Preview
              </button>
              <button
                type="button"
                className="host-context-item"
                onClick={() => {
                  const entry = contextMenu.entry;
                  setContextMenu(null);
                  void kickoffDownload(ready.sessionId, entry.path);
                }}
              >
                <Download size={14} /> Download
              </button>
            </>
          )}
          <button
            type="button"
            className="host-context-item"
            onClick={() => {
              const entry = contextMenu.entry;
              setContextMenu(null);
              void copyRemotePath(entry.path);
            }}
          >
            <Copy size={14} /> Copy path
          </button>
          <div className="host-context-sep" />
          <button
            type="button"
            className="host-context-item"
            onClick={() => {
              const entry = contextMenu.entry;
              setContextMenu(null);
              startMove(entry);
            }}
          >
            <FolderInput size={14} /> Move
          </button>
          <button
            type="button"
            className="host-context-item"
            onClick={() => {
              const entry = contextMenu.entry;
              setContextMenu(null);
              startRename(entry);
            }}
          >
            <Pencil size={14} /> Rename
          </button>
          <button
            type="button"
            className="host-context-item"
            onClick={() => {
              const entry = contextMenu.entry;
              setContextMenu(null);
              startChmod(entry);
            }}
          >
            <ShieldCheck size={14} /> Edit permissions
          </button>
          <div className="host-context-sep" />
          <button
            type="button"
            className="host-context-item danger"
            onClick={() => {
              const entry = contextMenu.entry;
              setContextMenu(null);
              startDelete(entry);
            }}
          >
            <Trash2 size={14} /> Delete{selectedPaths.has(contextMenu.entry.path) && selectedPaths.size > 1 ? ` ${selectedPaths.size} items` : ""}
          </button>
        </div>
      )}

      {preview && (
        <div className="dialog-backdrop sftp-preview-backdrop" onClick={() => setPreview(null)}>
          <div
            className="dialog sftp-preview-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={`Preview ${preview.entry.name}`}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="dialog-header">
              <div className="dialog-title">
                <h2>{preview.entry.name}</h2>
                <span>{preview.kind === "loading" ? "Preparing preview" : "Preview"}</span>
              </div>
              <button type="button" className="icon-tool" aria-label="Close preview" onClick={() => setPreview(null)}>
                <X size={14} />
              </button>
            </div>
            <div className="sftp-preview-path">
              {"path" in preview ? preview.path : preview.entry.path}
            </div>
            <div className="sftp-preview-body">
              {preview.kind === "loading" && (
                <div className="sftp-preview-empty">
                  <Loader2 size={16} className="spin" /> Preparing preview
                </div>
              )}
              {preview.kind === "error" && (
                <div className="sftp-preview-empty danger">{preview.message}</div>
              )}
              {preview.kind === "binary" && (
                <div className="sftp-preview-empty">
                  Binary preview is not available for this file. Size: {fmtBytes(preview.size)}.
                </div>
              )}
              {preview.kind === "image" && (
                <img src={preview.dataUrl} alt={preview.entry.name} />
              )}
              {preview.kind === "html" && (
                <iframe title={preview.entry.name} sandbox="" srcDoc={preview.text} />
              )}
              {preview.kind === "text" && (
                <pre>{preview.text}</pre>
              )}
            </div>
          </div>
        </div>
      )}

      <footer className="sftp-explorer-foot">
        <code>{listing.cwd}</code>
        <span>{listing.entries.length} item{listing.entries.length === 1 ? "" : "s"}</span>
      </footer>
    </div>
  );
}

function HostPicker({
  hosts,
  onConnect,
}: {
  hosts: HostRow[];
  onConnect: (host: HostRow) => void;
}) {
  const lastActivation = useRef<{ hostId: string; at: number } | null>(null);
  const activateHost = (host: HostRow) => {
    const now = Date.now();
    const last = lastActivation.current;
    if (last?.hostId === host.id && now - last.at < 750) return;
    lastActivation.current = { hostId: host.id, at: now };
    onConnect(host);
  };

  return (
    <div className="sftp-picker">
      <div className="sftp-picker-head">
        <Folder size={24} />
        <div>
          <h1>SFTP</h1>
          <p>Choose a saved host to browse remote files.</p>
        </div>
      </div>
      <div className="sftp-host-list">
        {hosts.length === 0 ? (
          <div className="sftp-empty-state">No saved hosts yet.</div>
        ) : hosts.map(host => (
          <button
            type="button"
            className="sftp-host-row"
            key={host.id}
            onClick={() => activateHost(host)}
            title="Open SFTP"
          >
            <OsBadge os={host.os ?? "linux"} size={32} />
            <div>
              <strong>{host.label}</strong>
              <span>{host.username}@{host.hostname}{host.port !== 22 ? `:${host.port}` : ""}</span>
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}
