import { useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowLeft, ArrowUp, Braces, Brain, Clock, FolderClosed, Palette, Upload, Search, Plus, Minus, ChevronRight, Play, RefreshCw, WandSparkles, Loader2, AlertCircle,
} from "lucide-react";
import { THEMES, findTheme } from "../themes";
import { api } from "../lib/api";
import { listCommandHistory, recordCommand, subscribeCommandHistory, type CommandHistoryItem } from "../lib/commandHistory";
import { getShellCwd } from "../lib/shellCwd";
import { FONT_OPTIONS, themeVarStyle, type Appearance, type FontId } from "../lib/appearance";
import {
  defaultPromptEnhancerSettings,
  loadPromptEnhancerApiKey,
  brainScopeKey,
  loadPromptEnhancerBrainId,
  loadPromptEnhancerSettings,
  normalizePromptEnhancerModel,
  OPENROUTER_MODEL_PRESETS,
  providerLabel,
  savePromptEnhancerApiKey,
  savePromptEnhancerBrainId,
  savePromptEnhancerSettings,
  validatePromptEnhancerModel,
  indexedRootsForHost,
  rememberIndexedRoot,
  forgetIndexedRoot,
  type PromptEnhancerSettings,
} from "../lib/promptEnhancer";
import type { BrainIndexAiConfig, BrainIndexProgress, BrainStatus, RemoteEntry, SnippetRow, HostRow, PromptEnhancerProvider } from "../types";

type DrawerTab = "snippets" | "history" | "enhancer" | "appearance";

/** What "project" the prompt enhancer should index when the toggle flips on.
 *  Computed at the App level from the active tab; null when there's nothing
 *  indexable in scope (e.g. user is on the host list). */
export type ProjectScope =
  | { kind: "local"; root: string }
  | { kind: "remote"; sessionId: string; hostId: string };

interface Props {
  appearance: Appearance;
  onChange: (next: Appearance) => void;
  activeSessionId: string | null;
  activeHost: HostRow | null;
  hosts: HostRow[];
  projectScope: ProjectScope | null;
  liveSessionIds?: string[];
  variant?: "full" | "collaborator-theme";
}

const TABS: Array<{ id: DrawerTab; icon: typeof Braces; label: string }> = [
  { id: "snippets",   icon: Braces,  label: "Snippets" },
  { id: "history",    icon: Clock,   label: "History" },
  { id: "enhancer",   icon: WandSparkles, label: "Prompt Enhancer" },
  { id: "appearance", icon: Palette, label: "Appearance" },
];

export function Drawer({ appearance, onChange, activeSessionId, activeHost, hosts, projectScope, liveSessionIds, variant = "full" }: Props) {
  const [tab, setTab] = useState<DrawerTab>("appearance");
  const canUploadToActiveSession = Boolean(
    activeSessionId && activeHost && activeHost.id !== "local-terminal" && activeHost.port !== 0,
  );

  // Scope the active theme's CSS variables to the drawer subtree only.
  // The app chrome (sidebar/topbar) stays locked to the dark default; this
  // lets the right panel adopt the active theme without bleeding into the
  // rest of the chrome.
  const themedStyle = useMemo(
    () => themeVarStyle(appearance.themeId) as React.CSSProperties,
    [appearance.themeId],
  );
  const themeMode = useMemo(() => findTheme(appearance.themeId).mode, [appearance.themeId]);

  if (variant === "collaborator-theme") {
    return (
      <aside
        className="drawer collab-theme-drawer"
        data-theme-scope={appearance.themeId}
        data-theme-mode={themeMode}
        style={themedStyle}
      >
        <CollaboratorThemePanel appearance={appearance} onChange={onChange} />
      </aside>
    );
  }

  return (
    <aside
      className="drawer"
      data-theme-scope={appearance.themeId}
      data-theme-mode={themeMode}
      style={themedStyle}
    >
      <nav className="drawer-tabs" role="tablist" aria-label="Drawer tabs">
        {canUploadToActiveSession && (
          <button
            type="button"
            className="dtab drawer-upload-action"
            title="Upload files or folders to active VPS"
            aria-label="Upload files or folders to active VPS"
            onClick={() => {
              window.dispatchEvent(new CustomEvent("tersh:upload-request", {
                detail: { sessionId: activeSessionId },
              }));
            }}
          >
            <Upload size={15} strokeWidth={1.75} />
          </button>
        )}
        {TABS.map(t => {
          const Icon = t.icon;
          const active = t.id === tab;
          return (
            <button
              key={t.id}
              role="tab"
              aria-selected={active}
              className={"dtab" + (active ? " active" : "")}
              onClick={() => setTab(t.id)}
              title={t.label}
            >
              <Icon size={15} strokeWidth={1.75} />
            </button>
          );
        })}
      </nav>

      <div className="drawer-body">
        {tab === "snippets"   && <SnippetsPanel activeSessionId={activeSessionId} />}
        {tab === "history"    && <HistoryPanel activeSessionId={activeSessionId} activeHost={activeHost} hosts={hosts} />}
        {tab === "enhancer"   && <PromptEnhancerPanel activeSessionId={activeSessionId} projectScope={projectScope} liveSessionIds={liveSessionIds} />}
        {tab === "appearance" && <AppearancePanel appearance={appearance} onChange={onChange} />}
      </div>
    </aside>
  );
}

function CollaboratorThemePanel({ appearance, onChange }: { appearance: Appearance; onChange: (next: Appearance) => void }) {
  return (
    <div className="collab-theme-picker" aria-label="Collaborator themes">
      <ul className="collab-theme-strip">
        {THEMES.map(t => {
          const active = t.id === appearance.themeId;
          return (
            <li key={t.id}>
              <button
                className={"collab-theme-chip" + (active ? " active" : "")}
                onClick={() => onChange({ ...appearance, themeId: t.id })}
                title={t.name}
                aria-pressed={active}
              >
                <ThemePreview themeId={t.id} active={active} />
                <span className="collab-theme-name">{t.name}</span>
                <span className="theme-card-tags">
                  <span className={"theme-card-pill " + t.mode}>{t.mode}</span>
                  {t.isNew && <span className="theme-card-pill new">New</span>}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

// ── SNIPPETS ───────────────────────────────────────────────────────────────
function SnippetsPanel({ activeSessionId }: { activeSessionId: string | null }) {
  const [snippets, setSnippets] = useState<SnippetRow[]>([]);
  const [query, setQuery] = useState("");
  const mounted = useRef(true);
  // Inline add mode: "list" shows the existing snippet list, "add" slides in
  // the create form. Clicking "New Snippet" no longer navigates away to the
  // full Snippets page.
  const [mode, setMode] = useState<"list" | "add">("list");

  const refresh = () => {
    api.listSnippets()
      .then(rows => { if (mounted.current) setSnippets(rows); })
      .catch(() => { if (mounted.current) setSnippets([]); });
  };
  useEffect(() => {
    mounted.current = true;
    refresh();
    return () => {
      mounted.current = false;
    };
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return snippets;
    return snippets.filter(s =>
      s.label.toLowerCase().includes(q) || s.command.toLowerCase().includes(q),
    );
  }, [snippets, query]);

  const runSnippet = (id: string) => {
    if (activeSessionId) api.runSnippet(activeSessionId, id);
  };

  if (mode === "add") {
    return (
      <SnippetForm
        onCancel={() => setMode("list")}
        onSaved={() => { setMode("list"); refresh(); }}
      />
    );
  }

  return (
    <>
      <header className="dpanel-head">
        <button
          className="dpanel-new"
          title="Create new snippet"
          onClick={() => setMode("add")}
        >
          <Braces size={12} strokeWidth={2} /> New Snippet
        </button>
        <div className="dpanel-search">
          <Search size={12} strokeWidth={2} />
          <input
            placeholder="Filter"
            value={query}
            onChange={e => setQuery(e.target.value)}
            spellCheck={false}
          />
        </div>
      </header>

      {filtered.length === 0 ? (
        <div className="dempty small">
          <Braces size={22} strokeWidth={1.5} />
          <p>{snippets.length === 0 ? "No snippets yet." : "No matches."}</p>
        </div>
      ) : (
        <ul className="dlist">
          {filtered.map(s => (
            <li key={s.id}>
              <button
                className="drow drow-snippet"
                onClick={() => runSnippet(s.id)}
                title={activeSessionId ? `Run "${s.label}" on active session` : "Connect to a host to run snippets"}
                disabled={!activeSessionId}
              >
                <span className="drow-mark"><Braces size={11} strokeWidth={2} /></span>
                <div className="drow-text">
                  <span className="drow-label">{s.label}</span>
                  <code className="drow-sub">{s.command}</code>
                </div>
                <span className="drow-action"><Play size={11} strokeWidth={2} /></span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

// Inline new-snippet form. Stays inside the drawer — no navigation away —
// so the user can quickly capture a command they just typed without losing
// the terminal context. Layout uses a back arrow + title + Save in the
// header, then an Action-description field above the Script field.
function SnippetForm({ onCancel, onSaved }: { onCancel: () => void; onSaved: () => void }) {
  const [label, setLabel] = useState("");
  const [command, setCommand] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const canSave = label.trim().length > 0 && command.trim().length > 0 && !saving;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const save = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!canSave) return;
    setSaving(true);
    setError(null);
    try {
      await api.addSnippet({
        label: label.trim(),
        command,
        description: null,
        tags: null,
      });
      if (!mounted.current) return;
      onSaved();
    } catch (err) {
      if (!mounted.current) return;
      setError(String(err));
      setSaving(false);
    }
  };

  return (
    <form className="snippet-form" onSubmit={save}>
      <header className="snippet-form-head">
        <button
          type="button"
          className="snippet-form-back"
          onClick={onCancel}
          aria-label="Back to snippets"
        >
          <ArrowLeft size={14} strokeWidth={2} />
        </button>
        <div className="snippet-form-title">
          <span>New Snippet</span>
          <span className="snippet-form-sub">Personal</span>
        </div>
        <button
          type="submit"
          className="snippet-form-save"
          disabled={!canSave}
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </header>

      <label className="snippet-field">
        <span className="snippet-field-label">Action description</span>
        <input
          type="text"
          placeholder="Example: check network load"
          value={label}
          onChange={e => setLabel(e.target.value)}
          autoFocus
          spellCheck={false}
        />
      </label>

      <label className="snippet-field snippet-field-script">
        <span className="snippet-field-label">Script <span className="snippet-field-required">*</span></span>
        <textarea
          placeholder="ssh-keygen -lf ~/.ssh/id_ed25519.pub"
          value={command}
          onChange={e => setCommand(e.target.value)}
          spellCheck={false}
          rows={6}
        />
      </label>

      {error && <div className="snippet-form-error">{error}</div>}
    </form>
  );
}

// ── HISTORY — unique commands run ──────────────────────────────────────────
function HistoryPanel({ activeSessionId, activeHost, hosts }: { activeSessionId: string | null; activeHost: HostRow | null; hosts: HostRow[] }) {
  const [items, setItems] = useState<CommandHistoryItem[]>(() => listCommandHistory());

  useEffect(() => {
    return subscribeCommandHistory(() => setItems(listCommandHistory()));
  }, []);

  const hostLabel = (item: CommandHistoryItem) =>
    hosts.find(h => h.id === item.hostId)?.label ?? item.hostLabel;

  const runCommand = (command: string) => {
    if (!activeSessionId) return;
    api.sendInput(activeSessionId, `${command}\r`);
    if (activeHost) recordCommand(command, activeHost);
  };

  if (items.length === 0) {
    return (
      <div className="dempty small">
        <Clock size={22} strokeWidth={1.5} />
        <p>No commands yet. Run commands in a terminal and they will appear here.</p>
      </div>
    );
  }

  return (
    <>
      <ul className="dlist">
        {items.map(item => (
          <li key={item.command}>
            <button
              className="drow drow-log"
              onClick={() => runCommand(item.command)}
              disabled={!activeSessionId}
              title={activeSessionId ? "Run command in active terminal" : "Connect to a terminal to run commands"}
            >
              <span className="drow-mark"><Clock size={11} strokeWidth={2} /></span>
              <div className="drow-text">
                <code className="drow-label">{item.command}</code>
                <span className="drow-sub">{hostLabel(item)} · {fmtTime(item.at)}</span>
              </div>
              <span className="drow-action"><Play size={11} strokeWidth={2} /></span>
            </button>
          </li>
        ))}
      </ul>
    </>
  );
}

function fmtTime(seconds: number): string {
  const d = new Date(seconds * 1000);
  const diff = (Date.now() - d.getTime()) / 1000;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

// ── PROMPT ENHANCER ────────────────────────────────────────────────────────
const PROVIDERS: PromptEnhancerProvider[] = ["openrouter", "deepseek", "mimo", "custom"];

/// Transient Project Index state that must be ISOLATED per VPS connection
/// (per session) — indexing/browse/scan running on one VPS must never show on
/// another, and connections (even multiple to the same VPS) index concurrently.
/// Keyed by session id; the panel renders the active connection's slice while
/// background indexing keeps updating its own slice.
interface ConnIndexState {
  indexBusy: boolean;
  indexProgress: BrainIndexProgress | null;
  indexError: string;
  remoteProjects: string[];
  remoteProjectsLoading: boolean;
  selectedRemoteRoot: string;
  browseOpen: boolean;
  browsePath: string;
  browseEntries: RemoteEntry[];
  browseLoading: boolean;
}
const EMPTY_CONN: ConnIndexState = {
  indexBusy: false,
  indexProgress: null,
  indexError: "",
  remoteProjects: [],
  remoteProjectsLoading: false,
  selectedRemoteRoot: "",
  browseOpen: false,
  browsePath: "",
  browseEntries: [],
  browseLoading: false,
};

const promptEnhancerConnCache: Record<string, ConnIndexState> = {};
const promptEnhancerScannedSessions = new Set<string>();
const promptEnhancerScanRequests = new Map<string, Promise<string[]>>();
// A hydrated remote index older than this auto re-syncs on reconnect (the
// backend re-checks the same age gate; the frontend only decides whether to
// bother asking). Matches RECONNECT_REFRESH_AFTER_SECS on the backend.
const RECONNECT_STALE_SECS = 5 * 60;

function PromptEnhancerPanel({
  activeSessionId,
  projectScope,
  liveSessionIds,
}: {
  activeSessionId: string | null;
  projectScope: ProjectScope | null;
  liveSessionIds?: string[];
}) {
  const [settings, setSettingsState] = useState<PromptEnhancerSettings>(() => loadPromptEnhancerSettings());
  const [apiKey, setApiKeyState] = useState(() => loadPromptEnhancerApiKey());
  const configured = apiKey.trim().length > 0 && settings.model.trim().length > 0;
  // "Advanced" (model override + base URL) stays collapsed by default. The API
  // key is the only field most users touch, and it lives outside this toggle.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [brains, setBrains] = useState<BrainStatus[]>([]);
  const [brainsLoaded, setBrainsLoaded] = useState(false);
  const [activeBrainId, setActiveBrainIdRaw] = useState<string | null>(null);
  // Stable per-VPS / per-local-project key. The selected project is scoped to
  // the active connection, so switching VPS terminals switches the selection
  // (and Ctrl+P on each terminal sends ITS own VPS's project, not a shared one).
  const brainScope = useMemo(
    () =>
      projectScope
        ? brainScopeKey(
            projectScope.kind,
            projectScope.kind === "remote" ? projectScope.sessionId : null,
            projectScope.kind === "local" ? projectScope.root : null,
          )
        : null,
    [projectScope],
  );
  // Load this connection's saved selection whenever the active scope changes.
  useEffect(() => {
    setActiveBrainIdRaw(loadPromptEnhancerBrainId(brainScope));
  }, [brainScope]);
  // Live mirror of the active scope. selectBrainForScope must compare a job's
  // captured scope against the CURRENT active scope (not the render-time one it
  // closed over, which would always equal the captured value — a no-op guard).
  const brainScopeRef = useRef(brainScope);
  useEffect(() => { brainScopeRef.current = brainScope; }, [brainScope]);
  // Per-connection transient Project Index state. The key is the session id, so
  // each VPS connection (even multiple to the same VPS) has its own indexing,
  // browse, scan and error state, and they run concurrently. The panel shows
  // the ACTIVE connection's slice; background indexing keeps updating its own.
  const [conns, setConns] = useState<Record<string, ConnIndexState>>(() => ({ ...promptEnhancerConnCache }));
  const connKey = activeSessionId ?? "";
  const conn = conns[connKey] ?? EMPTY_CONN;
  const patchConn = (key: string, patch: Partial<ConnIndexState>) => {
    if (!key) return;
    setConns(prev => {
      const nextConn = { ...(prev[key] ?? promptEnhancerConnCache[key] ?? EMPTY_CONN), ...patch };
      promptEnhancerConnCache[key] = nextConn;
      return { ...prev, [key]: nextConn };
    });
  };
  // Read aliases for the active connection (keeps the JSX unchanged).
  const {
    indexBusy, indexProgress, indexError, remoteProjects, remoteProjectsLoading,
    selectedRemoteRoot, browseOpen, browsePath, browseEntries, browseLoading,
  } = conn;
  // Active-connection setters used by the JSX.
  const setIndexError = (v: string) => patchConn(connKey, { indexError: v });
  const setBrowseOpen = (v: boolean) => patchConn(connKey, { browseOpen: v });
  const setSelectedRemoteRoot = (v: string | ((prev: string) => string)) =>
    patchConn(connKey, { selectedRemoteRoot: typeof v === "function" ? v(conn.selectedRemoteRoot) : v });

  const panelMounted = useRef(true);
  // Per-session: in-flight index guard (state lags a render) and SFTP-listing
  // sequence counters, so isolated connections don't interfere. unlistens holds
  // every live progress subscription so they're all torn down on unmount.
  const inFlightSessions = useRef<Set<string>>(new Set());
  const browseSeqs = useRef<Record<string, number>>({});
  const unlistens = useRef<Set<() => void>>(new Set());
  useEffect(() => {
    panelMounted.current = true;
    return () => {
      panelMounted.current = false;
      for (const off of unlistens.current) off();
      unlistens.current.clear();
    };
  }, []);

  // Associate a brain with a specific host scope (captured at the start of an
  // async index run, so a job that finishes after the user switched VPS still
  // saves to the RIGHT host, and only updates the displayed selection if that
  // scope is still the active one).
  const selectBrainForScope = (scope: string | null, brainId: string | null) => {
    savePromptEnhancerBrainId(scope, brainId);
    if (scope === brainScopeRef.current) setActiveBrainIdRaw(brainId);
  };
  const selectBrainId = (brainId: string | null) => {
    setIndexError("");
    selectBrainForScope(brainScope, brainId);
  };

  const refreshBrains = async (): Promise<BrainStatus[]> => {
    try {
      const list = await api.brainList();
      setBrains(list);
      return list;
    } catch {
      setBrains([]);
      return [];
    } finally {
      setBrainsLoaded(true);
    }
  };

  useEffect(() => {
    refreshBrains();
  }, []);

  // Hydrate the provider API key from the vault (persisted across launches).
  // sessionStorage is only a same-session cache; the vault is the source of
  // truth, so a fresh launch with an empty cache still recovers the key.
  useEffect(() => {
    let cancelled = false;
    api.promptEnhancerGetApiKey()
      .then(stored => {
        if (cancelled || !stored) return;
        setApiKeyState(prev => {
          if (prev.trim()) return prev; // user already typed one this session
          savePromptEnhancerApiKey(stored);
          return stored;
        });
      })
      .catch(() => {/* no persisted key, or vault unavailable — leave as-is */});
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!brainsLoaded || !activeBrainId || indexBusy) return;
    if (brains.some(b => b.id === activeBrainId)) return;
    // Do not auto-select a project from the active terminal. Augment-style
    // indexing must be explicit: the user chooses which project brain is
    // active, then Ctrl+P uses that brain_id.
    selectBrainId(null);
    setIndexError("Selected project index was no longer available. Choose or rebuild the project index.");
  }, [activeBrainId, brains, brainsLoaded, indexBusy]);

  const currentBrain: BrainStatus | undefined = useMemo(() => {
    if (!activeBrainId) return undefined;
    return brains.find(b => b.id === activeBrainId);
  }, [activeBrainId, brains]);

  const projectIndexOn = !!currentBrain;

  // Required to enable the toggle. The agent uses the SAME chat model with
  // tool calls — no separate embedding setup. Just need a valid chat config.
  const aiReady = apiKey.trim().length > 0 && settings.model.trim().length > 0;
  const brainAiConfig = (): BrainIndexAiConfig => {
    const model = normalizePromptEnhancerModel(settings.provider, settings.model);
    validatePromptEnhancerModel(settings.provider, model);
    return {
      provider: settings.provider,
      base_url: settings.baseUrl.trim() || null,
      api_key: apiKey,
      model,
      embedding_model: settings.embeddingModel.trim() || null,
    };
  };

  // Scan the VPS for project roots ONCE per connection — cached in that
  // session's slice, so switching tabs doesn't re-scan / re-flash "Scanning VPS…".
  useEffect(() => {
    if (projectScope?.kind !== "remote" || projectIndexOn) return;
    const sessionId = projectScope.sessionId;
    const hostId = projectScope.hostId;
    let cancelled = false;
    const applyRemoteProjects = (list: string[]) => {
      const cur = promptEnhancerConnCache[sessionId] ?? EMPTY_CONN;
      const shellCwd = getShellCwd(sessionId);
      const selectedRemoteRoot = cur.selectedRemoteRoot && list.includes(cur.selectedRemoteRoot)
        ? cur.selectedRemoteRoot
        : shellCwd && list.includes(shellCwd)
          ? shellCwd
          : "";
      const nextConn = {
        ...cur,
        remoteProjects: list,
        selectedRemoteRoot,
        remoteProjectsLoading: false,
      };
      promptEnhancerConnCache[sessionId] = nextConn;
      if (!cancelled) {
        setConns(prev => ({ ...prev, [sessionId]: nextConn }));
      }
    };
    if (promptEnhancerScannedSessions.has(sessionId)) {
      const cached = promptEnhancerConnCache[sessionId];
      if (cached && !cancelled) setConns(prev => ({ ...prev, [sessionId]: cached }));
      return () => { cancelled = true; };
    }
    promptEnhancerScannedSessions.add(sessionId);
    patchConn(sessionId, { remoteProjectsLoading: true });
    const request = promptEnhancerScanRequests.get(sessionId) ?? api.brainListRemoteProjects(sessionId);
    promptEnhancerScanRequests.set(sessionId, request);
    request
      .then(list => {
        applyRemoteProjects(list);
        // Hydrate any already-indexed projects on THIS VPS from <root>/.tersh/,
        // so reconnecting shows them as indexed without rebuilding. Check the
        // discovered roots, the live shell cwd, and roots we've indexed before.
        const shellCwd = getShellCwd(sessionId);
        const roots = [...list, ...(shellCwd ? [shellCwd] : []), ...indexedRootsForHost(hostId)];
        if (roots.length) {
          api.brainHydrateRemote(sessionId, roots)
            .then(async n => {
              if (n > 0 && !cancelled) {
                await refreshBrains();
                // Fire-and-forget incremental re-sync so a reconnected project
                // is current, not just loaded. Gated on age backend-side.
                if (!cancelled) void reconnectResyncRemote(sessionId, hostId);
              }
            })
            .catch(() => {/* nothing indexed on this VPS yet — ignore */});
        }
      })
      .catch(() => {
        promptEnhancerScannedSessions.delete(sessionId); // allow a retry on next focus
        const nextConn = {
          ...(promptEnhancerConnCache[sessionId] ?? EMPTY_CONN),
          remoteProjects: [],
          remoteProjectsLoading: false,
        };
        promptEnhancerConnCache[sessionId] = nextConn;
        if (!cancelled) setConns(prev => ({ ...prev, [sessionId]: nextConn }));
      })
      .finally(() => {
        promptEnhancerScanRequests.delete(sessionId);
        if (!cancelled) patchConn(sessionId, { remoteProjectsLoading: false });
      });
    return () => { cancelled = true; };
  }, [projectScope, projectIndexOn]);

  // Evict per-connection state for sessions that have closed, so conns and the
  // parallel refs don't grow unbounded over a long session of opening terminals.
  useEffect(() => {
    if (!liveSessionIds) return;
    const live = new Set(liveSessionIds);
    setConns(prev => {
      let changed = false;
      const next: Record<string, ConnIndexState> = {};
      for (const [k, v] of Object.entries(prev)) {
        if (live.has(k)) next[k] = v;
        else changed = true;
      }
      return changed ? next : prev;
    });
    for (const id of [...promptEnhancerScannedSessions]) if (!live.has(id)) promptEnhancerScannedSessions.delete(id);
    for (const id of Object.keys(promptEnhancerConnCache)) if (!live.has(id)) delete promptEnhancerConnCache[id];
    for (const id of [...promptEnhancerScanRequests.keys()]) if (!live.has(id)) promptEnhancerScanRequests.delete(id);
    for (const id of [...inFlightSessions.current]) if (!live.has(id)) inFlightSessions.current.delete(id);
    for (const id of Object.keys(browseSeqs.current)) if (!live.has(id)) delete browseSeqs.current[id];
    // (cwd eviction lives in App.tsx — see pruneShellCwds there — so it runs even
    // when this Drawer is unmounted.)
  }, [liveSessionIds]);

  const disabledReason = !projectScope
    ? "Open a terminal or SSH session first"
    : !apiKey.trim()
      ? "Set the provider API key below"
      : !settings.model.trim()
        ? "Set the chat model below"
        : null;

  const toggleEnabled = !!projectScope && (projectIndexOn || (aiReady && !disabledReason)) && !indexBusy;

  // Subscribe to live index progress for a SESSION's slice; self-cleans on
  // done and registers in `unlistens` for unmount teardown. Lets indexing on
  // one connection update only its own slice while you're viewing another.
  const subscribeIndexProgress = async (sessionKey: string, eventId: string): Promise<(() => void) | null> => {
    // The callback only updates progress; the caller's finally owns the single
    // teardown (exactly-once, even if the unlisten ever stops being idempotent).
    const handle = await api.onBrainIndexProgress(eventId, p => {
      if (panelMounted.current) patchConn(sessionKey, { indexProgress: p });
    });
    unlistens.current.add(handle);
    return handle;
  };

  // Index a specific remote folder with live progress, isolated to that
  // session's slice. Shared by the On toggle (selected root) and Browse.
  const runRemoteIndex = async (sessionId: string, root: string | null) => {
    if (!aiReady || inFlightSessions.current.has(sessionId)) return;
    inFlightSessions.current.add(sessionId);
    const targetScope = brainScope; // this session's host scope (captured)
    patchConn(sessionId, { indexError: "", indexBusy: true, indexProgress: null });
    let off: (() => void) | null = null;
    try {
      // Pre-allocate an id so we can subscribe to progress BEFORE indexing
      // starts (the real brain_id doesn't exist yet).
      const indexId = crypto.randomUUID();
      off = await subscribeIndexProgress(sessionId, indexId);
      const response = await api.brainEnableRemote(sessionId, root, brainAiConfig(), indexId);
      await refreshBrains();
      selectBrainForScope(targetScope, response.brain_id);
      // Remember this folder as indexed on this VPS, so a future reconnect can
      // hydrate it from <root>/.tersh/ even if discovery wouldn't surface it.
      if (root && root.trim() && projectScope?.kind === "remote") {
        rememberIndexedRoot(projectScope.hostId, root.trim());
      }
      patchConn(sessionId, { browseOpen: false });
    } catch (err) {
      patchConn(sessionId, { indexError: err instanceof Error ? err.message : String(err) });
    } finally {
      if (off) { off(); unlistens.current.delete(off); }
      patchConn(sessionId, { indexProgress: null, indexBusy: false });
      inFlightSessions.current.delete(sessionId);
    }
  };

  const enableIndex = async () => {
    if (!projectScope || !aiReady) return;
    if (projectScope.kind === "remote") {
      const autoRoot = selectedRemoteRoot || getShellCwd(projectScope.sessionId) || null;
      await runRemoteIndex(projectScope.sessionId, autoRoot);
      return;
    }
    const sessionKey = connKey;
    if (inFlightSessions.current.has(sessionKey)) return;
    inFlightSessions.current.add(sessionKey);
    const targetScope = brainScope;
    patchConn(sessionKey, { indexError: "", indexBusy: true, indexProgress: null });
    try {
      const response = await api.brainEnableLocal(projectScope.root, brainAiConfig());
      await refreshBrains();
      selectBrainForScope(targetScope, response.brain_id);
    } catch (err) {
      patchConn(sessionKey, { indexError: err instanceof Error ? err.message : String(err) });
    } finally {
      patchConn(sessionKey, { indexProgress: null, indexBusy: false });
      inFlightSessions.current.delete(sessionKey);
    }
  };

  // ── Browse VPS folder picker ──────────────────────────────────────────────
  const loadBrowse = async (sessionId: string, path: string) => {
    const seq = (browseSeqs.current[sessionId] ?? 0) + 1;
    browseSeqs.current[sessionId] = seq;
    patchConn(sessionId, { browseLoading: true, indexError: "" });
    try {
      const listing = await api.sftpListRemote(sessionId, path);
      if (seq !== browseSeqs.current[sessionId]) return; // superseded by a newer nav
      patchConn(sessionId, { browsePath: listing.cwd, browseEntries: listing.entries.filter(e => e.is_dir) });
    } catch (err) {
      if (seq === browseSeqs.current[sessionId]) patchConn(sessionId, { indexError: err instanceof Error ? err.message : String(err) });
    } finally {
      if (seq === browseSeqs.current[sessionId]) patchConn(sessionId, { browseLoading: false });
    }
  };

  const openBrowse = () => {
    if (projectScope?.kind !== "remote") return;
    patchConn(projectScope.sessionId, { browseOpen: true });
    // Start where you actually are: prefer the live shell cwd (OSC 7), then the
    // selected project root, else home (~).
    const start = getShellCwd(projectScope.sessionId) || selectedRemoteRoot || "~";
    void loadBrowse(projectScope.sessionId, start);
  };

  const browseUp = () => {
    if (projectScope?.kind !== "remote") return;
    const parent = browsePath.replace(/\/+$/, "").replace(/\/[^/]*$/, "") || "/";
    void loadBrowse(projectScope.sessionId, parent);
  };

  const addProjectIndex = async () => {
    if (!aiReady) {
      setIndexError(disabledReason ?? "Set the provider API key first.");
      return;
    }
    if (projectScope?.kind === "remote") {
      await enableIndex();
      return;
    }
    const sessionKey = connKey;
    if (inFlightSessions.current.has(sessionKey)) return;
    inFlightSessions.current.add(sessionKey);
    const targetScope = brainScope;
    patchConn(sessionKey, { indexError: "", indexBusy: true });
    try {
      const folder = await api.pickFolder();
      if (!folder) return;
      const response = await api.brainEnableLocal(folder, brainAiConfig());
      await refreshBrains();
      selectBrainForScope(targetScope, response.brain_id);
    } catch (err) {
      patchConn(sessionKey, { indexError: err instanceof Error ? err.message : String(err) });
    } finally {
      patchConn(sessionKey, { indexBusy: false });
      inFlightSessions.current.delete(sessionKey);
    }
  };

  const disableIndex = async () => {
    if (!currentBrain) return;
    const sessionKey = connKey;
    if (inFlightSessions.current.has(sessionKey)) return;
    inFlightSessions.current.add(sessionKey);
    const targetScope = brainScope;
    patchConn(sessionKey, { indexError: "", indexBusy: true });
    try {
      await api.brainDisable(currentBrain.id);
      selectBrainForScope(targetScope, null);
      // Stop remembering this folder as indexed on this VPS.
      if (currentBrain.scope.kind === "remote" && projectScope?.kind === "remote") {
        forgetIndexedRoot(projectScope.hostId, currentBrain.scope.remote_root);
      }
      await refreshBrains();
    } catch (err) {
      patchConn(sessionKey, { indexError: err instanceof Error ? err.message : String(err) });
    } finally {
      patchConn(sessionKey, { indexBusy: false });
      inFlightSessions.current.delete(sessionKey);
    }
  };

  const refreshIndex = async () => {
    if (!currentBrain) return;
    const sessionKey = connKey;
    if (inFlightSessions.current.has(sessionKey)) return;
    inFlightSessions.current.add(sessionKey);
    patchConn(sessionKey, { indexError: "", indexBusy: true, indexProgress: null });
    const brainId = currentBrain.id;
    const isRemote = currentBrain.scope.kind === "remote";
    let off: (() => void) | null = null;
    try {
      // Refresh keys progress on the real brain_id (it already exists).
      if (isRemote) off = await subscribeIndexProgress(sessionKey, brainId);
      await api.brainRefresh(brainId, aiReady ? brainAiConfig() : null);
      await refreshBrains();
    } catch (err) {
      patchConn(sessionKey, { indexError: err instanceof Error ? err.message : String(err) });
    } finally {
      if (off) { off(); unlistens.current.delete(off); }
      patchConn(sessionKey, { indexProgress: null, indexBusy: false });
      inFlightSessions.current.delete(sessionKey);
    }
  };

  // Auto re-sync a project's index on reconnect, incrementally. Picks the stale
  // remote brain for THIS host that matches the live shell cwd (or the only one)
  // and re-syncs it via the backend (which gates on age + a shared in-flight
  // guard, binds the EXACT session, and re-embeds changed files when the key is
  // present). NEVER touches selection — leaving it empty keeps the
  // "no-longer-available" guard untouched. Silent, like hydrate.
  const reconnectResyncRemote = async (sessionId: string, hostId: string) => {
    if (inFlightSessions.current.has(sessionId)) return;
    inFlightSessions.current.add(sessionId); // claim now — close the check-then-add gap
    // Reflect busy from the claim, so a same-session Off-control click during the
    // brain-list lookup is visibly blocked rather than silently dropped by the
    // inFlightSessions guard. The finally clears it.
    patchConn(sessionId, { indexError: "", indexBusy: true, indexProgress: null });
    let off: (() => void) | null = null;
    try {
      const list = await refreshBrains();
      const cwd = getShellCwd(sessionId);
      const stale = list.filter(b =>
        b.scope.kind === "remote" &&
        b.scope.host_id === hostId &&
        Date.now() / 1000 - b.indexed_at > RECONNECT_STALE_SECS,
      );
      const picked =
        stale.find(b => b.scope.kind === "remote" && b.scope.remote_root === cwd) ??
        (stale.length === 1 ? stale[0] : null);
      if (!picked) return; // nothing stale, or ambiguous — finally releases the slot
      patchConn(sessionId, { indexError: "", indexBusy: true, indexProgress: null });
      const ai = aiReady ? brainAiConfig() : null;
      off = await subscribeIndexProgress(sessionId, picked.id);
      await api.brainReconnectResync(sessionId, picked.id, ai);
      await refreshBrains();
    } catch {
      // Background re-sync failure shouldn't toast — same posture as hydrate.
    } finally {
      if (off) { off(); unlistens.current.delete(off); }
      patchConn(sessionId, { indexProgress: null, indexBusy: false });
      inFlightSessions.current.delete(sessionId);
    }
  };

  const setSettings = (next: PromptEnhancerSettings) => {
    setSettingsState(next);
    savePromptEnhancerSettings(next);
  };
  const setApiKey = (next: string) => {
    setApiKeyState(next);
    savePromptEnhancerApiKey(next); // fast in-session cache
    // Persist encrypted at rest in the vault so it survives launches.
    void api.promptEnhancerSetApiKey(next).catch(() => {/* cache still holds it this session */});
  };

  const changeProvider = (provider: PromptEnhancerProvider) => {
    setSettings(defaultPromptEnhancerSettings(provider));
  };

  // Enhancement runs from the terminal input via Ctrl+P (handled in
  // TerminalView). This panel is config only — no in-panel test box.
  const fireCtrlP = () => {
    window.dispatchEvent(new CustomEvent("tersh:prompt-enhance-request", {
      detail: { sessionId: activeSessionId },
    }));
  };

  return (
    <div className="prompt-panel">
      <header className="prompt-panel-status" data-state={configured ? "ok" : "needs"}>
        <span className="prompt-panel-status-dot" aria-hidden />
        <div className="prompt-panel-status-text">
          {configured ? (
            <>
              <strong>{providerLabel(settings.provider)}</strong>
              <code>{settings.model}</code>
            </>
          ) : (
            <span>Provider not configured</span>
          )}
        </div>
        <button
          type="button"
          className="dpanel-new"
          disabled={!activeSessionId}
          onClick={fireCtrlP}
          title={activeSessionId ? "Enhance active terminal input (Ctrl+P)" : "Open a terminal first"}
        >
          <WandSparkles size={12} strokeWidth={2} /> Ctrl+P
        </button>
      </header>

      <section className="prompt-section prompt-section-provider" aria-label="AI provider">
        <div className="prompt-section-eyebrow">Provider</div>
        <div className="prompt-seg" role="radiogroup" aria-label="Provider">
          {PROVIDERS.map(p => (
            <button
              key={p}
              type="button"
              role="radio"
              aria-checked={settings.provider === p}
              className={"prompt-seg-btn" + (settings.provider === p ? " active" : "")}
              onClick={() => changeProvider(p)}
            >
              {providerLabel(p)}
            </button>
          ))}
        </div>

        <label className="snippet-field prompt-apikey">
          <span className="snippet-field-label">API key</span>
          <input
            type="password"
            placeholder={`Paste your ${providerLabel(settings.provider)} key`}
            value={apiKey}
            onChange={e => setApiKey(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
        </label>

        <button
          type="button"
          className="prompt-settings-toggle"
          onClick={() => setSettingsOpen(o => !o)}
          aria-expanded={settingsOpen}
        >
          <ChevronRight size={12} strokeWidth={2} className={"dexpand-chev" + (settingsOpen ? " open" : "")} />
          <span>Advanced</span>
        </button>

        {settingsOpen && (
          <div className="prompt-settings">
            <label className="snippet-field">
              <span className="snippet-field-label">Model</span>
              {settings.provider === "deepseek" ? (
                <select
                  value={settings.model || "deepseek-v4-flash"}
                  onChange={e => setSettings({ ...settings, model: e.target.value })}
                >
                  <option value="deepseek-v4-flash">deepseek-v4-flash</option>
                  <option value="deepseek-v4-pro">deepseek-v4-pro</option>
                </select>
              ) : settings.provider === "openrouter" ? (
                <OpenRouterModelPicker
                  value={settings.model}
                  onChange={model => setSettings({ ...settings, model })}
                />
              ) : (
                <input
                  type="text"
                  placeholder="provider/model"
                  value={settings.model}
                  onChange={e => setSettings({ ...settings, model: e.target.value })}
                  spellCheck={false}
                />
              )}
            </label>

            {(settings.provider === "custom" || settings.provider === "mimo" || settings.baseUrl.trim()) && (
              <label className="snippet-field">
                <span className="snippet-field-label">Base URL</span>
                <input
                  type="url"
                  placeholder="https://api.example.com/v1"
                  value={settings.baseUrl}
                  onChange={e => setSettings({ ...settings, baseUrl: e.target.value })}
                  spellCheck={false}
                />
              </label>
            )}
          </div>
        )}
      </section>

      <section className="prompt-section prompt-section-index" aria-label="Project Index">
      <div
        className={
          "prompt-index" +
          (projectIndexOn ? " on" : "") +
          (!projectScope ? " disabled" : "")
        }
      >
        <div className="prompt-index-head">
          <div className="prompt-index-title">
            <Brain size={12} strokeWidth={2} />
            <span>Project Index</span>
          </div>
          <button
            type="button"
            className={"prompt-index-switch" + (projectIndexOn ? " on" : "")}
            onClick={projectIndexOn ? disableIndex : enableIndex}
            disabled={!toggleEnabled}
            aria-pressed={projectIndexOn}
            title={
              projectIndexOn
                ? "Turn indexing off"
                : disabledReason
                  ? disabledReason
                  : "Index this project so Ctrl+P retrieves only relevant chunks"
            }
          >
            <span className="prompt-index-switch-track" aria-hidden>
              <span className="prompt-index-switch-thumb" />
            </span>
            <span>{projectIndexOn ? "On" : "Off"}</span>
          </button>
        </div>
        <div className="prompt-index-scopeline">
          {currentBrain ? brainScopeLabel(currentBrain) : projectScope ? scopeLabel(projectScope) : "No project in scope"}
        </div>
        {projectScope?.kind === "remote" && !projectIndexOn && (
          <>
            <label className="prompt-index-remote-pick">
              <span className="prompt-index-remote-pick-label">
                VPS project {remoteProjects.length > 0 && "(detected)"}
                {remoteProjectsLoading && (
                  <Loader2 size={11} strokeWidth={2.25} className="spin" aria-label="Scanning VPS" />
                )}
              </span>
              <select
                value={selectedRemoteRoot}
                onChange={e => {
                  const root = e.target.value;
                  setSelectedRemoteRoot(root);
                  // Selecting a project IS the action — index it immediately,
                  // no separate On step. Empty placeholder is a no-op.
                  if (root && projectScope.kind === "remote") void runRemoteIndex(projectScope.sessionId, root);
                }}
                disabled={remoteProjectsLoading || indexBusy}
              >
                <option value="">
                  {remoteProjectsLoading
                    ? "Scanning…"
                    : remoteProjects.length > 0
                      ? "Select a project to index…"
                      : "No projects detected — Browse to pick"}
                </option>
                {remoteProjects.map(root => (
                  <option key={root} value={root}>{root}</option>
                ))}
              </select>
            </label>
            <button
              type="button"
              className="prompt-index-browse-toggle"
              onClick={() => (browseOpen ? setBrowseOpen(false) : openBrowse())}
              disabled={indexBusy}
              aria-expanded={browseOpen}
            >
              <FolderClosed size={12} strokeWidth={1.75} />
              {browseOpen ? "Hide browser" : "Browse VPS…"}
            </button>
            {browseOpen && (
              <div className="prompt-index-browser">
                <div className="prompt-index-browser-bar">
                  <button
                    type="button"
                    className="ghost icon-only"
                    onClick={browseUp}
                    disabled={browseLoading || !browsePath || browsePath === "/"}
                    title="Up one folder"
                  >
                    <ArrowUp size={12} strokeWidth={2} />
                  </button>
                  <code className="prompt-index-browser-path" title={browsePath}>{browsePath || "…"}</code>
                </div>
                <div className="prompt-index-browser-list">
                  {browseLoading ? (
                    <div className="prompt-index-status"><span><Loader2 size={11} strokeWidth={2.25} className="spin" /> Loading…</span></div>
                  ) : browseEntries.length === 0 ? (
                    <div className="prompt-index-status"><span>No sub-folders here</span></div>
                  ) : (
                    browseEntries.map(entry => (
                      <button
                        key={entry.path}
                        type="button"
                        className="prompt-index-browser-row"
                        onClick={() => { if (projectScope.kind === "remote") void loadBrowse(projectScope.sessionId, entry.path); }}
                        title={entry.path}
                      >
                        <FolderClosed size={12} strokeWidth={1.75} />
                        <span>{entry.name}</span>
                        <ChevronRight size={11} strokeWidth={2} />
                      </button>
                    ))
                  )}
                </div>
                <button
                  type="button"
                  className="prompt-index-browser-pick"
                  onClick={() => { if (projectScope.kind === "remote" && browsePath) void runRemoteIndex(projectScope.sessionId, browsePath); }}
                  disabled={indexBusy || browseLoading || !browsePath || !aiReady}
                  title={aiReady ? "Index this folder" : "Set the provider API key first"}
                >
                  <Brain size={12} strokeWidth={2} /> Index this folder
                </button>
              </div>
            )}
          </>
        )}
        {(indexProgress || (indexBusy && projectScope?.kind === "remote")) && (
          <div className="prompt-index-progress">
            <div className="prompt-index-status">
              <span>
                <Loader2 size={11} strokeWidth={2.25} className="spin" />
                {indexProgress && indexProgress.total > 0
                  ? ` Indexing… ${Math.round((indexProgress.processed / indexProgress.total) * 100)}%`
                  : " Scanning project…"}
              </span>
            </div>
            <div className={"term-upload-bar" + (!indexProgress || indexProgress.total === 0 ? " indeterminate" : "")}>
              <div
                className="term-upload-fill"
                style={{ width: `${indexProgress && indexProgress.total > 0 ? Math.round((indexProgress.processed / indexProgress.total) * 100) : 0}%` }}
              />
            </div>
          </div>
        )}
        {projectIndexOn && currentBrain ? (
          <>
            <div className="prompt-index-selected">
              <div>
                <span className="snippet-field-label">Selected context</span>
                <strong>{brainScopeLabel(currentBrain)}</strong>
                <code>{brainScopeDetail(currentBrain)}</code>
              </div>
              <div className="prompt-index-selected-actions">
                <button
                  type="button"
                  className="ghost icon-only"
                  onClick={refreshIndex}
                  disabled={indexBusy}
                  title="Refresh project index"
                >
                  <RefreshCw size={11} strokeWidth={2} className={indexBusy ? "spin" : ""} />
                </button>
                <button
                  type="button"
                  className="ghost icon-only"
                  onClick={disableIndex}
                  disabled={indexBusy}
                  title="Remove selected project index"
                >
                  <Minus size={11} strokeWidth={2} />
                </button>
              </div>
            </div>
            <div className="prompt-index-status">
              <span>
                Indexed {currentBrain.files_indexed} files / {currentBrain.chunks_indexed} chunks
                {currentBrain.last_used_at > 0 ? ` · last used ${formatAgo(currentBrain.last_used_at)}` : ""}
              </span>
            </div>
            {currentBrain.has_embeddings && currentBrain.embeddings_stale_since ? (
              <div className="prompt-index-status prompt-index-stale">
                <span>
                  <AlertCircle size={11} strokeWidth={2.25} />
                  {" "}Embeddings stale since {formatAgo(currentBrain.embeddings_stale_since)}
                  {currentBrain.embedding_model ? ` (${currentBrain.embedding_model})` : ""}
                  {aiReady
                    ? " — Refresh to re-embed."
                    : " — Set the API key, then Refresh."}
                </span>
              </div>
            ) : null}
            {(currentBrain.project_digest || currentBrain.overview || currentBrain.languages.length > 0 || currentBrain.frameworks.length > 0 || currentBrain.capabilities.length > 0 || currentBrain.architecture.length > 0 || currentBrain.modules.length > 0) && (
              <div className="prompt-index-summary">
                {(currentBrain.project_digest || currentBrain.overview) && (
                  <p>{currentBrain.project_digest || currentBrain.overview}</p>
                )}
                {(currentBrain.capabilities.length > 0 || currentBrain.architecture.length > 0 || currentBrain.modules.length > 0 || currentBrain.frameworks.length > 0 || currentBrain.languages.length > 0) && (
                  <div className="prompt-index-tags">
                    {[
                      ...currentBrain.capabilities,
                      ...currentBrain.architecture,
                      ...currentBrain.modules,
                      ...currentBrain.frameworks,
                      ...currentBrain.languages,
                    ].slice(0, 10).map(tag => (
                      <span key={tag}>{tag}</span>
                    ))}
                  </div>
                )}
              </div>
            )}
          </>
        ) : disabledReason && projectScope ? (
          <div className="prompt-index-status disabled-reason">
            <span>{disabledReason}</span>
          </div>
        ) : null}
        {projectScope?.kind === "local" && !projectIndexOn && (
          <button
            type="button"
            className="prompt-index-browse-toggle"
            onClick={addProjectIndex}
            disabled={indexBusy}
            title="Pick a local folder to index"
          >
            <FolderClosed size={12} strokeWidth={1.75} />
            Choose a folder…
          </button>
        )}
        {indexError && <div className="prompt-index-error">{indexError}</div>}
      </div>
      </section>

    </div>
  );
}

// ── PROJECT INDEX helpers (used by Prompt Enhancer above) ──────────────────
function scopeLabel(scope: ProjectScope): string {
  if (scope.kind === "local") {
    const last = scope.root.split("/").filter(Boolean).pop() ?? scope.root;
    return last;
  }
  return "current SSH project";
}

function brainScopeLabel(brain: BrainStatus): string {
  if (brain.scope.kind === "local") {
    const last = brain.scope.root.split("/").filter(Boolean).pop() ?? brain.scope.root;
    return last;
  }
  return brain.scope.remote_root.split("/").filter(Boolean).pop() || brain.scope.remote_root;
}

function brainScopeDetail(brain: BrainStatus): string {
  if (brain.scope.kind === "local") {
    return brain.scope.root;
  }
  return brain.scope.remote_root;
}

// Dropdown of popular OpenRouter chat models for the prompt enhancer, with a
// "Custom" escape that reveals a text input. The picker is presets-only when
// the current model matches one; switches to text mode for anything else.
function OpenRouterModelPicker({
  value,
  onChange,
}: {
  value: string;
  onChange: (next: string) => void;
}) {
  const isPreset = OPENROUTER_MODEL_PRESETS.some(p => p.value === value);
  const [customMode, setCustomMode] = useState(!isPreset && value.length > 0);

  return (
    <>
      <select
        value={customMode ? "__custom__" : value || OPENROUTER_MODEL_PRESETS[0]?.value || ""}
        onChange={e => {
          const v = e.target.value;
          if (v === "__custom__") {
            setCustomMode(true);
          } else {
            setCustomMode(false);
            onChange(v);
          }
        }}
      >
        {OPENROUTER_MODEL_PRESETS.map(p => (
          <option key={p.value} value={p.value}>
            {p.label} — {p.value}
          </option>
        ))}
        <option value="__custom__">Custom (type below)</option>
      </select>
      {customMode && (
        <input
          type="text"
          placeholder="e.g. provider/model"
          value={value}
          onChange={e => onChange(e.target.value)}
          spellCheck={false}
          autoFocus
        />
      )}
    </>
  );
}

function formatAgo(unixSec: number): string {
  const diff = Math.max(0, Date.now() / 1000 - unixSec);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return new Date(unixSec * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

// ── APPEARANCE — font + size + themes ──────────────────────────────────────
function AppearancePanel({ appearance, onChange }: { appearance: Appearance; onChange: (next: Appearance) => void }) {
  const [fontOpen, setFontOpen] = useState(false);
  const currentFont = FONT_OPTIONS.find(f => f.id === appearance.fontId);

  return (
    <>
      {/* Font as a click-to-expand row. */}
      <button
        className="dexpand-row"
        onClick={() => setFontOpen(o => !o)}
        aria-expanded={fontOpen}
      >
        <span className="dexpand-label">Font</span>
        <span className="dexpand-value" style={{ fontFamily: currentFont?.stack }}>
          {currentFont?.label}
        </span>
        <ChevronRight size={12} strokeWidth={2} className={"dexpand-chev" + (fontOpen ? " open" : "")} />
      </button>
      {fontOpen && (
        <ul className="dlist dlist-inset">
          {FONT_OPTIONS.map(f => (
            <li key={f.id}>
              <button
                className={"drow drow-font" + (f.id === appearance.fontId ? " active" : "")}
                onClick={() => onChange({ ...appearance, fontId: f.id as FontId })}
                style={{ fontFamily: f.stack }}
              >
                <span className="drow-label">{f.label}</span>
                <span className="drow-preview">AaBbCc 0123</span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {/* Size stepper — compact row */}
      <div className="dexpand-row static">
        <span className="dexpand-label">Text size</span>
        <div className="size-stepper">
          <button
            onClick={() => onChange({ ...appearance, fontSize: Math.max(8, appearance.fontSize - 1) })}
            aria-label="Smaller"
          ><Minus size={12} strokeWidth={2} /></button>
          <span className="value">{appearance.fontSize}</span>
          <button
            onClick={() => onChange({ ...appearance, fontSize: Math.min(28, appearance.fontSize + 1) })}
            aria-label="Larger"
          ><Plus size={12} strokeWidth={2} /></button>
        </div>
      </div>

      <h3 className="dsection-head">Themes</h3>
      <ul className="theme-grid">
        {THEMES.map(t => {
          const active = t.id === appearance.themeId;
          return (
            <li key={t.id}>
              <button
                className={"theme-card" + (active ? " active" : "")}
                onClick={() => onChange({ ...appearance, themeId: t.id })}
                title={t.name}
              >
                <ThemePreview themeId={t.id} active={active} />
                <div className="theme-card-meta">
                  <span className="theme-card-name">{t.name}</span>
                  <span className="theme-card-tags">
                    <span className={"theme-card-pill " + t.mode}>{t.mode}</span>
                    {t.isNew && <span className="theme-card-pill new">New</span>}
                  </span>
                </div>
              </button>
            </li>
          );
        })}
      </ul>
    </>
  );
}

// Chunky terminal preview that uses the actual theme's xterm palette so the
// card actually looks like the terminal will look. Three bars stand in for
// prompt, output, output. When the
// card is selected, a tight ANSI-swatch strip slides in along the bottom
// of the preview (red / yellow / green / blue / magenta / cyan) so the
// user can see the actual palette without committing.
function ThemePreview({ themeId, active }: { themeId: string; active: boolean }) {
  const theme = findTheme(themeId).xterm;
  return (
    <div className={"theme-preview" + (active ? " active" : "")} style={{ background: theme.background }}>
      <div className="theme-preview-bars">
        <span className="theme-preview-bar" style={{ background: theme.green,   width: "62%" }} />
        <span className="theme-preview-bar" style={{ background: theme.yellow,  width: "78%" }} />
        <span className="theme-preview-bar" style={{ background: theme.cyan,    width: "48%" }} />
      </div>
      {active && (
        <div className="theme-preview-swatches" aria-hidden="true">
          <span style={{ background: theme.red }} />
          <span style={{ background: theme.yellow }} />
          <span style={{ background: theme.green }} />
          <span style={{ background: theme.blue }} />
          <span style={{ background: theme.magenta }} />
          <span style={{ background: theme.cyan }} />
        </div>
      )}
    </div>
  );
}
