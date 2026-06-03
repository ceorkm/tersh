import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { Folder, Palette, PanelRight, X } from "lucide-react";
import { TopBar } from "./components/TopBar";
import { Sidebar } from "./components/Sidebar";
import { HostGrid } from "./components/HostGrid";
import { HostInspector } from "./components/HostInspector";
import { TabStrip } from "./components/TabStrip";
import { ConnectingView } from "./components/ConnectingView";
import { TerminalView } from "./components/TerminalView";
import { Collaborator } from "./components/Collaborator";
import { Drawer, type ProjectScope } from "./components/Drawer";
import { CommandPalette } from "./components/CommandPalette";
import { SftpPage } from "./components/SftpPage";
import { KeychainPage } from "./components/KeychainPage";
import { SnippetsPage } from "./components/SnippetsPage";
import { TunnelsPage } from "./components/TunnelsPage";
import { KnownHostsPage } from "./components/KnownHostsPage";
import { OsBadge } from "./assets/os-icons";
import { api } from "./lib/api";
import { savePromptEnhancerApiKey } from "./lib/promptEnhancer";
import { pruneShellCwds } from "./lib/shellCwd";
import { listen } from "@tauri-apps/api/event";
import type { HostRow, OsKind, SidebarSection, Tab } from "./types";
import {
  applyAppearance,
  DEFAULT_FONT,
  DEFAULT_FONT_SIZE,
  loadAppearance,
  saveAppearance,
  themeVarStyle,
  type Appearance,
} from "./lib/appearance";

// localStorage key for an offline list of hosts. The Tauri backend handles persistence
// in production; this fallback lets the UI render even when Tauri isn't running
// (e.g. opening dist/index.html in a browser, dev iteration).
const FALLBACK_HOSTS_KEY = "tersh:fallback-hosts";
const OS_KINDS: ReadonlySet<OsKind> = new Set([
  "ubuntu", "debian", "fedora", "arch", "alpine",
  "centos", "rhel", "apple", "windows", "bsd", "linux",
]);
const APP_CHROME_APPEARANCE: Appearance = {
  themeId: "tersh-dark",
  fontId: DEFAULT_FONT,
  fontSize: DEFAULT_FONT_SIZE,
};
const LOCAL_TERMINAL_HOST: HostRow = {
  id: "local-terminal",
  label: "Terminal",
  hostname: "localhost",
  port: 0,
  username: "local",
  auth_kind: "password",
  key_path: null,
  group_name: null,
  os: "apple",
};

function normalizeFallbackHost(value: unknown): HostRow | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  const id = typeof row.id === "string" && row.id.trim() ? row.id : crypto.randomUUID();
  const hostname = typeof row.hostname === "string" ? row.hostname.trim() : "";
  const username = typeof row.username === "string" ? row.username.trim() : "";
  if (!hostname || !username) return null;
  const port = typeof row.port === "number" && Number.isInteger(row.port) && row.port >= 1 && row.port <= 65_535
    ? row.port
    : 22;
  const authKind = row.auth_kind === "key_file" ? "key_file" : "password";
  const os = typeof row.os === "string" && OS_KINDS.has(row.os as OsKind) ? row.os as OsKind : "linux";
  return {
    id,
    label: typeof row.label === "string" && row.label.trim() ? row.label.trim() : hostname,
    hostname,
    port,
    username,
    auth_kind: authKind,
    key_path: typeof row.key_path === "string" && row.key_path.trim() ? row.key_path : null,
    group_name: typeof row.group_name === "string" && row.group_name.trim() ? row.group_name : null,
    os,
    jump_host_id: typeof row.jump_host_id === "string" && row.jump_host_id.trim() ? row.jump_host_id : null,
    env_json: typeof row.env_json === "string" && row.env_json.trim() ? row.env_json : null,
    startup_snippet: typeof row.startup_snippet === "string" && row.startup_snippet.trim() ? row.startup_snippet : null,
  };
}

function loadFallbackHosts(): HostRow[] {
  try {
    const raw = localStorage.getItem(FALLBACK_HOSTS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map(normalizeFallbackHost)
      .filter((host): host is HostRow => host !== null);
  } catch {}
  return [];
}
function saveFallbackHosts(hosts: HostRow[]) {
  try {
    localStorage.setItem(
      FALLBACK_HOSTS_KEY,
      JSON.stringify(hosts.map(normalizeFallbackHost).filter(Boolean)),
    );
  } catch {}
}

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

const VALID_SECTIONS: SidebarSection[] = ["hosts", "sftp", "keychain", "tunnels", "snippets", "known-hosts"];
const sectionFromHash = (): SidebarSection => {
  const h = window.location.hash.replace(/^#/, "") as SidebarSection;
  return VALID_SECTIONS.includes(h) ? h : "hosts";
};

export function App() {
  const [section, _setSection] = useState<SidebarSection>(() => sectionFromHash());
  const setSection = (s: SidebarSection) => { _setSection(s); window.location.hash = s; };
  const [hosts, setHosts] = useState<HostRow[]>([]);
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);

  // Latest tabs/hosts available to async event callbacks so they don't
  // operate on stale closure snapshots (which was breaking host-key-changed
  // notifications during connect — see RC-9).
  const tabsRef = useRef<Tab[]>(tabs);
  useEffect(() => { tabsRef.current = tabs; }, [tabs]);
  const hostsRef = useRef<HostRow[]>(hosts);
  useEffect(() => { hostsRef.current = hosts; }, [hosts]);

  // Seed the prompt-enhancer API key from the vault at APP STARTUP (not lazily
  // when the enhancer panel first mounts), so Ctrl+P and the reconnect index
  // re-sync both see the key from the first connect — independent of which
  // drawer tab is open. The panel keeps its own copy for live edits.
  useEffect(() => {
    api.promptEnhancerGetApiKey()
      .then(k => { if (k) savePromptEnhancerApiKey(k); })
      .catch(() => {/* no persisted key, or vault unavailable */});
  }, []);
  useEffect(() => {
    setCollaboratorTabIds(prev => prev.filter(id =>
      tabs.some(t => t.id === id && t.kind === "local")
    ));
  }, [tabs]);

  // Per-tab reconnect-attempt counter. Incremented to invalidate any pending
  // backoff timer. If the user
  // closes the tab or a fresh connection succeeds, stale timers bail out.
  const reconnectAttempt = useRef<Map<string, number>>(new Map());
  // Per-session unlisten for the `ssh://<sid>/disconnected` event.
  const disconnectListeners = useRef<Map<string, () => void>>(new Map());
  const localCwdListeners = useRef<Map<string, () => void>>(new Map());
  const terminalReplay = useRef<Map<string, { chunks: Uint8Array[]; bytes: number }>>(new Map());
  const [addDialogOpen, setAddDialogOpen] = useState(false);
  const [editingHost, setEditingHost] = useState<HostRow | null>(null);
  const [search, setSearch] = useState("");
  const [drawerOpen, setDrawerOpen] = useState(true);
  const [collaboratorMode, setCollaboratorMode] = useState(false);
  const [collaboratorTabIds, setCollaboratorTabIds] = useState<string[]>([]);
  const [collaboratorRoots, setCollaboratorRoots] = useState<string[]>([]);
  const [collaboratorExplorerOpen, setCollaboratorExplorerOpen] = useState(true);
  const [collaboratorFileExpanded, setCollaboratorFileExpanded] = useState<Set<string>>(() => new Set());
  const collaboratorKnownRoots = useRef<Set<string>>(new Set());
  // One live terminal appearance source. The right drawer is a global terminal
  // preference, so switching theme/font/size must affect every terminal, not
  // only whichever SSH host happens to be active.
  const [defaultAppearance, setDefaultAppearance] = useState<Appearance>(() => loadAppearance());
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [sftpInitialHost, setSftpInitialHost] = useState<HostRow | null>(null);
  const [sftpLauncherOpen, setSftpLauncherOpen] = useState(false);
  const lastSftpOpenRef = useRef<{ hostId: string; at: number } | null>(null);

  const leaveSession = () => {
    setCollaboratorMode(false);
    setActiveTabId(null);
    setSftpLauncherOpen(false);
  };

  const commitTabs = (next: Tab[] | ((prev: Tab[]) => Tab[])) => {
    setTabs(prev => {
      const resolved = typeof next === "function" ? next(prev) : next;
      tabsRef.current = resolved;
      return resolved;
    });
  };

  useEffect(() => {
    let cancelled = false;
    const wantedSessionIds = new Set<string>();
    for (const tab of tabs) {
      if (tab.kind !== "local" || tab.state.kind !== "connected") continue;
      const sessionId = tab.state.sessionId;
      wantedSessionIds.add(sessionId);
      if (localCwdListeners.current.has(sessionId)) continue;
      api.onLocalTerminalCwd(sessionId, (cwd) => {
        const currentTab = tabsRef.current.find(t =>
          t.kind === "local" &&
          t.state.kind === "connected" &&
          t.state.sessionId === sessionId
        );
        if (!currentTab) return;
        if (currentTab.localCwd && normalizeLocalPath(currentTab.localCwd) === normalizeLocalPath(cwd)) return;
        updateTab(currentTab.id, { localCwd: cwd });
      })
        .then((off) => {
          if (cancelled || !wantedSessionIds.has(sessionId)) {
            off();
            return;
          }
          localCwdListeners.current.set(sessionId, off);
        })
        .catch(() => {});
    }
    for (const [sessionId, off] of localCwdListeners.current) {
      if (wantedSessionIds.has(sessionId)) continue;
      off();
      localCwdListeners.current.delete(sessionId);
    }
    return () => {
      cancelled = true;
    };
  }, [tabs]);

  useEffect(() => {
    return () => {
      for (const [, off] of localCwdListeners.current) off();
      localCwdListeners.current.clear();
    };
  }, []);

  useEffect(() => {
    const onHash = () => {
      leaveSession();
      _setSection(sectionFromHash());
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  // Cmd+K / Ctrl+K opens command palette
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen(p => !p);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Host-key-changed listener — possible MitM or legitimate server rotation.
  // Backend has already rejected the connection and emitted this event. The
  // renderer can warn only; it cannot accept or overwrite host-key pins.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      try {
        const stop = await listen<{ host_id: string; presented: string; known: string[] }>(
          "ssh://host-key-changed",
          (e) => {
            const { host_id, presented, known } = e.payload;
            const host = hostsRef.current.find(h => h.id === host_id);
            const label = host ? `${host.label} (${host.hostname})` : host_id;
            const msg =
              `SSH HOST KEY CHANGED for ${label}\n\n` +
              `Presented:  ${presented}\n` +
              `Trusted:    ${known.join(", ")}\n\n` +
              `This could be a man-in-the-middle attack. Tersh blocked the connection.`;
            alert(msg);
          },
        );
        if (cancelled) stop();
        else unlisten = stop;
      } catch {
        // Not running under Tauri (browser dev mode)
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  const activeTab = useMemo(
    () => {
      const tab = tabs.find(t => t.id === activeTabId) ?? null;
      if (!collaboratorMode && tab && collaboratorTabIds.includes(tab.id)) return null;
      return tab;
    },
    [tabs, activeTabId, collaboratorMode, collaboratorTabIds],
  );

  // What project the Prompt Enhancer's "Project Index" toggle should index
  // when flipped on. Local terminals → the folder they opened in (or cwd if
  // the user has cd'd somewhere stable); SSH/SFTP → keyed on the host.
  // null when there's nothing indexable in scope.
  const projectScope: ProjectScope | null = useMemo(() => {
    if (!activeTab || activeTab.state.kind !== "connected") return null;
    if (activeTab.kind === "local") {
      const root = activeTab.localStartCwd ?? activeTab.localCwd ?? null;
      if (!root) return null;
      return { kind: "local", root };
    }
    if ((activeTab.kind ?? "ssh") === "ssh" || activeTab.kind === "sftp") {
      return {
        kind: "remote",
        sessionId: activeTab.state.sessionId,
        hostId: activeTab.host.id,
      };
    }
    return null;
  }, [activeTab]);

  // Session ids of all currently-connected tabs, so the Drawer can evict
  // per-connection Project Index state for connections that have closed.
  const liveSessionIds = useMemo(
    () => tabs.flatMap(t => (t.state.kind === "connected" ? [t.state.sessionId] : [])),
    [tabs],
  );

  // Evict OSC-7 cwd entries for dead sessions. Lives HERE (always mounted), not
  // in the Drawer — the Drawer unmounts when closed or on a non-terminal view,
  // and each reconnect mints a fresh session id, so a Drawer-scoped prune would
  // let the cwd map grow unbounded whenever the Drawer is closed.
  useEffect(() => {
    pruneShellCwds(new Set(liveSessionIds));
  }, [liveSessionIds]);

  // host_id -> session_id for tabs currently in "connected" state.
  // Consumed by TunnelsPage to enable per-host Start buttons.
  const activeSessionsByHost = useMemo(() => {
    const map = new Map<string, string>();
    for (const t of tabs) {
      if (t.state.kind === "connected" && (t.kind ?? "ssh") === "ssh") map.set(t.host.id, t.state.sessionId);
    }
    return map;
  }, [tabs]);

  const snippetSessionId = useMemo(() => {
    if (activeTab?.state.kind === "connected" && ((activeTab.kind ?? "ssh") === "ssh" || activeTab.kind === "local")) {
      return activeTab.state.sessionId;
    }
    const fallback = [...tabs].reverse().find(t =>
      t.state.kind === "connected" && ((t.kind ?? "ssh") === "ssh" || t.kind === "local")
    );
    return fallback?.state.kind === "connected" ? fallback.state.sessionId : null;
  }, [activeTab, tabs]);

  const appearanceForTab = (tab: Tab | null | undefined): Appearance => tab?.appearance ?? defaultAppearance;

  const appearance = useMemo(() => {
    if (collaboratorMode) {
      return defaultAppearance;
    }
    if (activeTab && activeTab.state.kind !== "idle") {
      return appearanceForTab(activeTab);
    }
    return APP_CHROME_APPEARANCE;
  }, [activeTab, collaboratorMode, defaultAppearance]);

  const updateAppearance = (next: Appearance) => {
    if (!collaboratorMode && activeTab && activeTab.state.kind !== "idle") {
      updateTab(activeTab.id, { appearance: next });
      return;
    }
    setDefaultAppearance(next);
    saveAppearance(next);
    try {
      new BroadcastChannel("tersh:appearance").postMessage(next);
    } catch {}
  };

  // Apply the ACTIVE tab's appearance to :root so the whole app — chrome
  // (sidebar/topbar), workspace, drawer, terminal — adopts the same theme
  // in lockstep. Picking a theme recolors the whole active session. Switching tabs
  // re-runs this with that tab's appearance. When no session is active
  // (Hosts page etc.) `appearance` falls back to APP_CHROME_APPEARANCE
  // so the host list stays in the neutral default look.
  useEffect(() => {
    applyAppearance(appearance);
  }, [appearance]);

  // Initial hosts load — Tauri backend if available, else fallback
  useEffect(() => {
    (async () => {
      try {
        const rows = await api.listHosts();
        setHosts(rows);
      } catch {
        setHosts(loadFallbackHosts());
      }
    })();
  }, []);

  const refreshHosts = async () => {
    try {
      const rows = await api.listHosts();
      setHosts(rows);
    } catch {
      setHosts(loadFallbackHosts());
    }
  };

  // ────────────────────────────────────────────────────────────────────────
  // RECONNECT LIFECYCLE
  // When the backend pump exits unexpectedly (network drop, server reboot,
  // keepalive failure), it emits `ssh://<sid>/disconnected`. We capture that
  // and run an exponential backoff: 2s → 4s → 8s, 8 attempts max. The user
  // can cancel by closing the tab; the attempt counter (`reconnectAttempt`)
  // invalidates any in-flight timer when it changes.
  // ────────────────────────────────────────────────────────────────────────

  const RECONNECT_DELAYS_MS = [2000, 2000, 4000, 4000, 8000, 8000, 8000, 8000];

  // Register the per-session disconnect listener *eagerly* — before the
  // setTabs() that promotes the tab to "connected". The [tabs] useEffect
  // also registers listeners, but it runs asynchronously after the render
  // commits; between setTabs and that re-run there is a tiny window where
  // a fast-dying backend session could fire /disconnected with no listener.
  // Calling this helper synchronously alongside the state transition closes
  // that window. The helper is idempotent (skips if already registered).
  const ensureDisconnectListener = (sid: string) => {
    // TOCTOU race fix: between this `has` check and the `set` after the
    // await, a second caller would also see "not registered" and start
    // its own subscription. Install a no-op sentinel synchronously so the
    // second caller's `has(sid)` short-circuits. The real `off` replaces
    // the sentinel after the await; on early return we delete the entry.
    if (disconnectListeners.current.has(sid)) return;
    disconnectListeners.current.set(sid, () => {});
    (async () => {
      try {
        const off = await api.onSessionDisconnected(sid, () => {
          const tab = tabsRef.current.find(t =>
            t.state.kind === "connected" &&
            (t.kind ?? "ssh") === "ssh" &&
            t.state.sessionId === sid,
          );
          if (!tab) return;
          startReconnect(tab.id, tab.host, sid);
        });
        // If the session was already retired while we were awaiting, drop
        // the listener immediately rather than leaking it into the map.
        if (!tabsRef.current.some(t =>
          t.state.kind === "connected" && t.state.sessionId === sid,
        )) {
          off();
          disconnectListeners.current.delete(sid);
          return;
        }
        // Replace the sentinel with the real unlisten.
        disconnectListeners.current.set(sid, off);
      } catch {
        // Tauri not running (browser dev mode). Clear sentinel so the
        // next call can retry rather than seeing a dead no-op.
        disconnectListeners.current.delete(sid);
      }
    })();
  };

  const reconnectDelay = (attempt: number): number =>
    RECONNECT_DELAYS_MS[Math.min(attempt - 1, RECONNECT_DELAYS_MS.length - 1)] ?? 8000;

  const attemptReconnect = async (tabId: string, host: HostRow, series: number, attempt: number) => {
    if (reconnectAttempt.current.get(tabId) !== series) return;
    const delay = reconnectDelay(attempt);
    await new Promise<void>(resolve => setTimeout(resolve, delay));
    if (reconnectAttempt.current.get(tabId) !== series) return;
    updateTab(tabId, { state: { kind: "connecting" } });
    try {
      const resp = await api.connect({
        host_id: host.id,
        auth_secret: null,
        cols: 100,
        rows: 30,
      });
      if (reconnectAttempt.current.get(tabId) !== series) {
        // User closed the tab while connect was in flight — clean up the
        // stranded backend session so the registry doesn't leak.
        api.disconnect(resp.session_id).catch(() => {});
        return;
      }
      // Register the disconnect listener BEFORE the state transition so we
      // never miss a fast /disconnected event from the new session.
      ensureDisconnectListener(resp.session_id);
      // Promote the new session id on the SSH tab through commitTabs so
      // async listeners and close/connect guards see the fresh state at once.
      commitTabs(prev => prev.map(t => {
        if (t.id === tabId) {
          return { ...t, state: { kind: "connected", sessionId: resp.session_id } };
        }
        return t;
      }));
    } catch (e) {
      if (reconnectAttempt.current.get(tabId) !== series) return;
      const message = String(e);
      const needsSecret = message.toLowerCase().includes("password required") ||
        message.toLowerCase().includes("passphrase") ||
        message.toLowerCase().includes("auth");
      if (needsSecret) {
        // No stored credentials — don't loop forever; show auth prompt.
        updateTab(tabId, {
          state: {
            kind: "auth_needed",
            reason: host.auth_kind === "password"
              ? "Saved credentials missing. Enter password to reconnect"
              : "Enter key passphrase to reconnect",
          },
        });
        return;
      }
      if (attempt >= RECONNECT_DELAYS_MS.length) {
        updateTab(tabId, {
          state: { kind: "error", message: `Reconnect failed after ${attempt} attempts: ${message}` },
        });
        return;
      }
      const nextDelay = reconnectDelay(attempt + 1);
      updateTab(tabId, {
        state: {
          kind: "reconnecting",
          attempt: attempt + 1,
          nextRetryAt: Date.now() + nextDelay,
          lastSessionId: "",
        },
      });
      // Recursive next-attempt. Surface any unexpected JS error (not an
      // SSH error caught above — a runtime bug in our own code) so the
      // tab isn't silently stuck in "reconnecting" forever.
      attemptReconnect(tabId, host, series, attempt + 1).catch(err => {
        // eslint-disable-next-line no-console
        console.error("reconnect attempt threw", err);
        if (reconnectAttempt.current.get(tabId) !== series) return;
        updateTab(tabId, { state: { kind: "error", message: `Reconnect failed: ${String(err)}` } });
      });
    }
  };

  const startReconnect = (tabId: string, host: HostRow, oldSessionId: string) => {
    // Clean up the dead backend session (best-effort; the pump already exited).
    api.disconnect(oldSessionId).catch(() => {});
    const series = (reconnectAttempt.current.get(tabId) ?? 0) + 1;
    reconnectAttempt.current.set(tabId, series);
    updateTab(tabId, {
      state: {
        kind: "reconnecting",
        attempt: 1,
        nextRetryAt: Date.now() + reconnectDelay(1),
        lastSessionId: oldSessionId,
      },
    });
    attemptReconnect(tabId, host, series, 1);
  };

  // Maintain a per-session `ssh://<sid>/disconnected` listener for every
  // SSH tab that's currently in the "connected" state. Re-runs when the set
  // of connected session-ids changes; uses a ref-stored Map so prior
  // listeners are preserved across renders and cleaned up only when the
  // session truly goes away.
  useEffect(() => {
    const connectedSids = new Set<string>();
    for (const t of tabs) {
      if (t.state.kind === "connected" && (t.kind ?? "ssh") === "ssh") {
        connectedSids.add(t.state.sessionId);
      }
    }
    // Register newcomers
    for (const sid of connectedSids) {
      ensureDisconnectListener(sid);
    }
    // Retire stale listeners
    for (const [sid, off] of Array.from(disconnectListeners.current.entries())) {
      if (!connectedSids.has(sid)) {
        off();
        disconnectListeners.current.delete(sid);
      }
    }
  }, [tabs]);

  // Tear down ALL registered disconnect listeners on component unmount.
  // The [tabs] effect above only retires listeners for sessions that have
  // left the tab list; on full unmount (app close, route teardown) it sets
  // `cancelled = true` but doesn't iterate the listener map. Without this
  // separate mount-once cleanup, every listener that was active at unmount
  // leaks across the Tauri event bridge into a torn-down React tree.
  useEffect(() => {
    return () => {
      for (const off of disconnectListeners.current.values()) {
        off();
      }
      disconnectListeners.current.clear();
    };
  }, []);

  useEffect(() => {
    let stops: Array<() => void> = [];
    let cancelled = false;
    (async () => {
      try {
        const [offFocus, offPending] = await Promise.all([
          listen<{ tabId: string }>("collab://terminal-focus", (e) => {
            if (e.payload?.tabId) setActiveTabId(e.payload.tabId);
          }),
          listen<{ tabId: string; chip: Tab["pendingChip"] }>("collab://pending-chip", (e) => {
            const tabId = e.payload?.tabId;
            if (!tabId) return;
            updateTab(tabId, { pendingChip: e.payload.chip });
          }),
        ]);
        stops = [offFocus, offPending];
        if (cancelled) {
          for (const stop of stops) stop();
          stops = [];
        }
      } catch {}
    })();
    return () => {
      cancelled = true;
      for (const stop of stops) stop();
    };
  }, []);

  const filteredHosts = useMemo(() => {
    if (!search.trim()) return hosts;
    const q = search.toLowerCase();
    return hosts.filter(h =>
      h.label.toLowerCase().includes(q) ||
      h.hostname.toLowerCase().includes(q) ||
      h.username.toLowerCase().includes(q) ||
      (h.group_name ?? "").toLowerCase().includes(q),
    );
  }, [hosts, search]);

  const isCollaboratorEligibleTab = (tab: Tab) => tab.kind === "local";

  const openCollaboratorMode = () => {
    const terminalIds = tabsRef.current.filter(isCollaboratorEligibleTab).map(t => t.id);
    if (terminalIds.length === 0) return;
    setCollaboratorTabIds(prev => {
      const existing = prev.filter(id => tabsRef.current.some(t => t.id === id && isCollaboratorEligibleTab(t)));
      const next = [...existing];
      for (const id of terminalIds) {
        if (!next.includes(id)) next.push(id);
      }
      return next;
    });
    setDrawerOpen(false);
    setCollaboratorMode(true);
  };

  const addCollaboratorTab = (tabId: string, options?: { assumeLocal?: boolean }) => {
    if (!options?.assumeLocal && !tabsRef.current.some(t => t.id === tabId && isCollaboratorEligibleTab(t))) return;
    setCollaboratorTabIds(prev => prev.includes(tabId) ? prev : [...prev, tabId]);
  };

  const closeCollaboratorMode = () => {
    setCollaboratorMode(false);
    setCollaboratorTabIds([]);
  };

  const connectHost = async (host: HostRow, secret: string | null = null, rememberCredential = false) => {
    // Normal SSH connections are normal tabs. They should not be swallowed by
    // Collaborator unless created through Collaborator's own controls.
    setCollaboratorMode(false);
    const displayHost = { ...host, label: host.label.trim() || host.hostname };
    const id = crypto.randomUUID();
    const newTab: Tab = {
      id,
      host: displayHost,
      kind: "ssh",
      appearance: defaultAppearance,
      state: { kind: "connecting" },
    };
    commitTabs(prev => [...prev, newTab]);
    setActiveTabId(id);
    let hasStoredPassword = false;
    try {
      hasStoredPassword = host.auth_kind === "password" && !secret
        ? await api.hasHostPassword(host.id).catch(() => false)
        : false;
      const resp = await api.connect({
        host_id: host.id,
        auth_secret: secret,
        cols: 100,
        rows: 30,
        remember_key_passphrase: rememberCredential,
      });
      // If the user closed the tab while api.connect was awaiting (multi-
      // second TCP handshake / auth), the new backend session would leak.
      // Tear it down and bail before touching tab state.
      if (!tabsRef.current.some(t => t.id === id)) {
        api.disconnect(resp.session_id).catch(() => {});
        return;
      }
      ensureDisconnectListener(resp.session_id);
      updateTab(id, { state: { kind: "connected", sessionId: resp.session_id } });
      refreshDetectedOs(displayHost, resp.session_id);
    } catch (e) {
      const message = String(e);
      const needsSecret = message.toLowerCase().includes("password required") ||
        message.toLowerCase().includes("passphrase") ||
        message.toLowerCase().includes("auth");
      updateTab(id, {
        state: needsSecret
          ? {
              kind: "auth_needed",
	              reason: host.auth_kind === "password" && hasStoredPassword
	                ? "Saved password failed. Enter password"
	                : host.auth_kind === "password"
	                  ? "No saved password yet. Enter once to save"
	                  : "Enter key passphrase (or leave blank)",
            }
          : { kind: "error", message },
      });
    }
  };

  const openHostTab = (host: HostRow) => {
    connectHost(host);
  };

  const retrySshTab = async (tabId: string, host: HostRow) => {
    updateTab(tabId, { state: { kind: "connecting" } });
    try {
      const resp = await api.connect({
        host_id: host.id,
        auth_secret: null,
        cols: 100,
        rows: 30,
      });
      if (!tabsRef.current.some(t => t.id === tabId)) {
        api.disconnect(resp.session_id).catch(() => {});
        return;
      }
      ensureDisconnectListener(resp.session_id);
      updateTab(tabId, { state: { kind: "connected", sessionId: resp.session_id } });
      refreshDetectedOs(host, resp.session_id);
    } catch (e) {
      const message = String(e);
      const needsSecret = message.toLowerCase().includes("password required") ||
        message.toLowerCase().includes("passphrase") ||
        message.toLowerCase().includes("auth");
      updateTab(tabId, {
        state: needsSecret
          ? {
              kind: "auth_needed",
              reason: host.auth_kind === "password"
                ? "No saved password yet. Enter once to save"
                : "Enter key passphrase (or leave blank)",
            }
          : { kind: "error", message },
      });
    }
  };

  const retryLocalTab = async (tabId: string, cwd?: string) => {
    updateTab(tabId, { state: { kind: "connecting" } });
    try {
      const resp = await api.startLocalTerminal(100, 30, cwd);
      if (!tabsRef.current.some(t => t.id === tabId)) {
        api.disconnect(resp.session_id).catch(() => {});
        return;
      }
      updateTab(tabId, { state: { kind: "connected", sessionId: resp.session_id } });
    } catch (e) {
      updateTab(tabId, { state: { kind: "error", message: String(e) } });
    }
  };

  const retrySftpTab = async (tabId: string, host: HostRow) => {
    updateTab(tabId, { state: { kind: "connecting" } });
    try {
      const resp = await api.connect({
        host_id: host.id,
        auth_secret: null,
        cols: 100,
        rows: 30,
      });
      if (!tabsRef.current.some(t => t.id === tabId)) {
        api.disconnect(resp.session_id).catch(() => {});
        return;
      }
      updateTab(tabId, { state: { kind: "connected", sessionId: resp.session_id } });
    } catch (e) {
      const message = String(e);
      const needsSecret = message.toLowerCase().includes("password required") ||
        message.toLowerCase().includes("passphrase") ||
        message.toLowerCase().includes("auth");
      updateTab(tabId, {
        state: needsSecret
          ? {
              kind: "auth_needed",
              reason: host.auth_kind === "password"
                ? "No saved password yet. Enter once to save"
                : "Enter key passphrase (or leave blank)",
            }
          : { kind: "error", message },
      });
    }
  };

  // Always spawn a fresh local terminal — no "focus existing" shortcut, so the
  // Terminal button always behaves predictably (one click = one new shell).
  // The previous behavior silently no-op'd when a
  // local tab already existed and that was the "I can't add more terminals"
  // complaint.
  const openLocalTerminal = async (cwd?: unknown, options?: { collaborator?: boolean }): Promise<string | null> => {
    const requestedCwd = typeof cwd === "string" && cwd.trim() ? cwd : null;
    const id = crypto.randomUUID();
    const tab: Tab = {
      id,
      host: LOCAL_TERMINAL_HOST,
      kind: "local",
      localCwd: requestedCwd ?? undefined,
      localStartCwd: requestedCwd ?? undefined,
      appearance: defaultAppearance,
      state: { kind: "connecting" },
    };
    commitTabs(prev => [...prev, tab]);
    if (options?.collaborator) {
      addCollaboratorTab(id, { assumeLocal: true });
      setCollaboratorMode(true);
    } else {
      setCollaboratorMode(false);
    }
    setActiveTabId(id);
    try {
      const resp = await api.startLocalTerminal(100, 30, requestedCwd);
      if (!tabsRef.current.some(t => t.id === id)) {
        api.disconnect(resp.session_id).catch(() => {});
        return null;
      }
      updateTab(id, { state: { kind: "connected", sessionId: resp.session_id } });
      return id;
    } catch (e) {
      updateTab(id, { state: { kind: "error", message: String(e) } });
      return id;
    }
  };

  const normalizeLocalPath = (path: string) => path.replace(/\/+$/, "") || "/";

  const openCollaboratorTerminal = () => {
    void openLocalTerminal(undefined, { collaborator: true });
  };

  const openFolderTerminal = async (options?: { collaborator?: boolean }) => {
    try {
      const folder = await api.pickFolder();
      if (!folder) return;
      if (options?.collaborator) {
        const picked = normalizeLocalPath(folder);
        const existingTab = tabsRef.current.find(tab =>
          collaboratorTabIds.includes(tab.id) &&
          tab.kind === "local" &&
          typeof tab.localCwd === "string" &&
          normalizeLocalPath(tab.localCwd) === picked
        );
        const alreadyOpen =
          collaboratorRoots.some(root => normalizeLocalPath(root) === picked) ||
          Boolean(existingTab);
        if (alreadyOpen) {
          setCollaboratorRoots(prev => prev.some(root => normalizeLocalPath(root) === picked) ? prev : [...prev, folder]);
          if (existingTab) setActiveTabId(existingTab.id);
          setCollaboratorMode(true);
          return;
        }
        setCollaboratorRoots(prev => prev.some(root => normalizeLocalPath(root) === picked) ? prev : [...prev, folder]);
        await openLocalTerminal(folder, options);
        setCollaboratorMode(true);
        return;
      }
      await openLocalTerminal(folder, options);
    } catch (e) {
      if (options?.collaborator) return;
      const id = crypto.randomUUID();
      commitTabs(prev => [...prev, {
        id,
        host: LOCAL_TERMINAL_HOST,
        kind: "local",
        appearance: defaultAppearance,
        state: { kind: "error", message: String(e) },
      }]);
      if (options?.collaborator) addCollaboratorTab(id, { assumeLocal: true });
      setActiveTabId(id);
    }
  };

  const openSftpFlow = (host?: HostRow | null) => {
    const targetHost = host ?? null;
    if (!targetHost) {
      if (collaboratorMode) {
        setCollaboratorMode(false);
        setActiveTabId(null);
        setSftpInitialHost(null);
        setSection("sftp");
        setSftpLauncherOpen(false);
        return;
      }
      setSftpLauncherOpen(true);
      return;
    }

    const now = Date.now();
    const lastOpen = lastSftpOpenRef.current;
    if (lastOpen?.hostId === targetHost.id && now - lastOpen.at < 750) {
      return;
    }
    lastSftpOpenRef.current = { hostId: targetHost.id, at: now };

    setSftpLauncherOpen(false);
    setCollaboratorMode(false);
    const displayHost = { ...targetHost, label: targetHost.label.trim() || targetHost.hostname };
    const id = crypto.randomUUID();
    commitTabs(prev => [...prev, {
      id,
      host: displayHost,
      kind: "sftp",
      sftpCwd: "~",
      appearance: defaultAppearance,
      state: { kind: "connecting" },
    }]);
    setActiveTabId(id);

    (async () => {
      let hasStoredPassword = false;
      try {
        hasStoredPassword = displayHost.auth_kind === "password"
          ? await api.hasHostPassword(displayHost.id).catch(() => false)
          : false;
        const resp = await api.connect({
          host_id: displayHost.id,
          auth_secret: null,
          cols: 100,
          rows: 30,
        });
        if (!tabsRef.current.some(t => t.id === id)) {
          api.disconnect(resp.session_id).catch(() => {});
          return;
        }
        updateTab(id, { state: { kind: "connected", sessionId: resp.session_id } });
      } catch (e) {
        const message = String(e);
        const needsSecret = message.toLowerCase().includes("password required") ||
          message.toLowerCase().includes("passphrase") ||
          message.toLowerCase().includes("auth");
        updateTab(id, {
          state: needsSecret
            ? {
                kind: "auth_needed",
                reason: displayHost.auth_kind === "password" && hasStoredPassword
                  ? "Saved password failed. Enter password"
                  : displayHost.auth_kind === "password"
                    ? "No saved password yet. Enter once to save"
                    : "Enter key passphrase (or leave blank)",
              }
            : { kind: "error", message },
        });
      }
    })();
  };

  const closeTab = (tabId: string) => {
    const closing = tabs.find(t => t.id === tabId);
    // Invalidate any in-flight reconnect series for this tab — stale timers
    // and pending api.connect calls will check this and bail out instead of
    // resurrecting a tab the user already closed.
    reconnectAttempt.current.set(tabId, (reconnectAttempt.current.get(tabId) ?? 0) + 1);
    let nextTabs = tabs.filter(t => t.id !== tabId);
    const isSshLike = (closing?.kind ?? "ssh") === "ssh";
    if (closing && isSshLike && closing.state.kind === "connected") {
      const closingSessionId = closing.state.sessionId;
      api.disconnect(closingSessionId).catch(() => {});
      terminalReplay.current.delete(tabId);
      commitTabs(nextTabs);
    } else if (closing && closing.kind === "sftp" && closing.state.kind === "connected") {
      api.disconnect(closing.state.sessionId).catch(() => {});
      commitTabs(nextTabs);
    } else if (closing && closing.kind === "local" && closing.state.kind === "connected") {
      api.disconnect(closing.state.sessionId).catch(() => {});
      terminalReplay.current.delete(tabId);
      commitTabs(nextTabs);
    } else if (closing && isSshLike && closing.state.kind === "reconnecting") {
      // Clean up the dead backend session if it's still hanging around.
      if (closing.state.lastSessionId) {
        api.disconnect(closing.state.lastSessionId).catch(() => {});
      }
      terminalReplay.current.delete(tabId);
      commitTabs(nextTabs);
    } else {
      terminalReplay.current.delete(tabId);
      commitTabs(nextTabs);
    }
    const nextCollaboratorIds = collaboratorTabIds.filter(id => id !== tabId);
    setCollaboratorTabIds(nextCollaboratorIds);
    if (activeTabId === tabId) {
      const nextCollaboratorTab = collaboratorMode
        ? nextTabs.find(t => nextCollaboratorIds.includes(t.id))
        : null;
      setActiveTabId(nextCollaboratorTab?.id ?? nextTabs[nextTabs.length - 1]?.id ?? null);
    }
  };

  const closeTabsForHost = (hostId: string) => {
    const currentTabs = tabsRef.current;
    const closingTabs = currentTabs.filter(t => t.host.id === hostId);
    if (closingTabs.length === 0) return;

    for (const tab of closingTabs) {
      reconnectAttempt.current.set(tab.id, (reconnectAttempt.current.get(tab.id) ?? 0) + 1);
      terminalReplay.current.delete(tab.id);
      if (tab.state.kind === "connected") {
        api.disconnect(tab.state.sessionId).catch(() => {});
      } else if (tab.state.kind === "reconnecting") {
        api.disconnect(tab.state.lastSessionId).catch(() => {});
      }
    }

    const nextTabs = currentTabs.filter(t => t.host.id !== hostId);
    commitTabs(nextTabs);
    setActiveTabId(current =>
      current && nextTabs.some(t => t.id === current)
        ? current
        : nextTabs[nextTabs.length - 1]?.id ?? null
    );
  };

  const updateTab = (tabId: string, patch: Partial<Tab>) => {
    commitTabs(prev => prev.map(t => t.id === tabId ? { ...t, ...patch } : t));
  };

  const rememberTerminalOutput = (tabId: string, chunk: Uint8Array) => {
    // Bounded frontend replay buffer. This keeps terminal contents visible
    // when switching between single-terminal and Mission Control without
    // turning the renderer into an unbounded log store.
    const MAX_BYTES = 2 * 1024 * 1024;
    const copy = chunk.slice();
    const entry = terminalReplay.current.get(tabId) ?? { chunks: [], bytes: 0 };
    entry.chunks.push(copy);
    entry.bytes += copy.byteLength;
    while (entry.bytes > MAX_BYTES && entry.chunks.length > 1) {
      const removed = entry.chunks.shift();
      entry.bytes -= removed?.byteLength ?? 0;
    }
    terminalReplay.current.set(tabId, entry);
  };

  const replayForTab = (tabId: string): Uint8Array[] =>
    terminalReplay.current.get(tabId)?.chunks ?? [];

  const hostToInput = (host: HostRow) => ({
    label: host.label,
    hostname: host.hostname,
    port: host.port,
    username: host.username,
    auth_kind: host.auth_kind,
    key_path: host.key_path,
    group_name: host.group_name,
    os: host.os ?? null,
    jump_host_id: host.jump_host_id ?? null,
    env_json: host.env_json ?? null,
    startup_snippet: host.startup_snippet ?? null,
  });

  const deleteHost = async (id: string) => {
    closeTabsForHost(id);
    try {
      await api.deleteHost(id);
    } catch (e) {
      if (!isTauriRuntime()) {
        const next = hostsRef.current.filter(h => h.id !== id);
        saveFallbackHosts(next);
        setHosts(next);
        return;
      }
      alert(`Delete host failed: ${e}`);
      await refreshHosts();
      return;
    }
    await refreshHosts();
  };

  const duplicateHost = async (host: HostRow) => {
    const input = hostToInput({ ...host, label: `${host.label || host.hostname} copy` });
    try {
      await api.addHost(input);
      await refreshHosts();
    } catch (e) {
      if (!isTauriRuntime()) {
        const copy: HostRow = { ...host, id: crypto.randomUUID(), label: input.label };
        const next = [...hosts, copy];
        saveFallbackHosts(next);
        setHosts(next);
        return;
      }
      alert(`Duplicate host failed: ${e}`);
      await refreshHosts();
    }
  };

  const refreshDetectedOs = (host: HostRow, sessionId: string) => {
    // Defer by ~750ms so the user's first interaction (SFTP open, drag-drop,
    // running a snippet) wins the SSH channel-open queue. detect_remote_os
    // opens its own exec channel; doing it immediately on connect put it in
    // direct contention with whatever the user actually wanted to do.
    // RC-1 already shortened the handle-mutex hold to just the channel open,
    // so this is belt-and-suspenders — keeps the connect path snappier.
    window.setTimeout(async () => {
      try {
        const current = hostsRef.current.find(h => h.id === host.id);
        if (!current) return;
        const sessionStillActive = tabsRef.current.some(t =>
          t.state.kind === "connected" &&
          t.state.sessionId === sessionId &&
          t.host.id === host.id,
        );
        if (!sessionStillActive) return;
        const os = await api.detectRemoteOs(sessionId);
        if (!os) return;
        const latest = hostsRef.current.find(h => h.id === host.id);
        if (!latest || os === latest.os) return;
        const next = { ...latest, os };
        await api.updateHost(host.id, hostToInput(next));
        setHosts(prev => prev.map(h => h.id === host.id ? next : h));
        commitTabs(prev => prev.map(t => t.host.id === host.id ? { ...t, host: next } : t));
      } catch {}
    }, 750);
  };

  const collaboratorTabs = tabs.filter(t => collaboratorTabIds.includes(t.id) && isCollaboratorEligibleTab(t));
  const collaboratorWorkspaceRoots = Array.from(new Set([
    ...collaboratorRoots,
    ...collaboratorTabs
      .filter(t => t.kind === "local" && typeof t.localCwd === "string" && t.localCwd.trim())
      .map(t => t.localCwd as string),
  ]));
  const normalTopbarTabs = tabs.filter(t => !collaboratorTabIds.includes(t.id));
  const persistentSessionTabs = tabs.filter(t =>
    !collaboratorTabIds.includes(t.id) &&
    t.state.kind === "connected" &&
    (((t.kind ?? "ssh") === "ssh") || t.kind === "local" || t.kind === "sftp")
  );
  const inSession = collaboratorMode || (activeTab !== null && activeTab.state.kind !== "idle");

  // Drawer (Snippets / History / Themes) appears for any
  // terminal session — SSH or local. Previously gated on activeIsSsh which
  // excluded the local shell, so the local Terminal tab had no drawer.
  const activeIsTerminal = activeTab?.state.kind === "connected" && ((activeTab.kind ?? "ssh") === "ssh" || activeTab.kind === "local");
  const inCollab = collaboratorMode;
  const terminalTabCount = collaboratorMode
    ? collaboratorTabs.length
    : tabs.filter(isCollaboratorEligibleTab).length;
  const canShowCollaborator = collaboratorMode || activeTab?.kind === "local";
  useEffect(() => {
    const currentRoots = new Set(collaboratorWorkspaceRoots);
    const newlySeen: string[] = [];
    for (const root of currentRoots) {
      if (!collaboratorKnownRoots.current.has(root)) {
        collaboratorKnownRoots.current.add(root);
        newlySeen.push(root);
      }
    }
    for (const root of Array.from(collaboratorKnownRoots.current)) {
      if (!currentRoots.has(root)) collaboratorKnownRoots.current.delete(root);
    }
    if (newlySeen.length === 0) return;
    setCollaboratorFileExpanded(prev => {
      const next = new Set(prev);
      for (const root of newlySeen) next.add(root);
      return next;
    });
  }, [collaboratorWorkspaceRoots.join("\n")]);

  return (
    <div className={"shell " + (inSession ? "in-session " : "") + (collaboratorMode ? "collaborator-session " : "") + (drawerOpen && activeIsTerminal ? "drawer-open" : "")}>

      <header className="topbar">
        <TopBar
          search={search}
          onSearch={setSearch}
          showSearch={!inSession}
          // Vaults / SFTP / Terminal pills + the open-session tabs are
          // permanent topbar chrome. The strip carries any active sessions so users can flip
          // between browsing hosts and live terminals without losing tabs.
          tabStrip={
            <TabStrip
              tabs={normalTopbarTabs}
              activeTabId={activeTabId}
              onActivate={(tabId) => {
                setCollaboratorMode(false);
                setSftpLauncherOpen(false);
                setActiveTabId(tabId);
              }}
              onClose={closeTab}
              // "+" in the tabstrip navigates to Hosts — it doesn't pop
              // a New Host dialog. From the Hosts page the user reaches
              // the form via "+ New host" themselves. Avoids hijacking
              // their flow with a modal.
              onNewTab={() => {
                leaveSession();
                setSftpInitialHost(null);
                setSection("hosts");
              }}
              onOpenVaults={() => {
                setSftpInitialHost(null);
                leaveSession();
                setSection("hosts");
              }}
              onOpenSftp={() => openSftpFlow(null)}
              sftpEnabled
              collaboratorMode={inCollab}
              collaboratorCount={collaboratorTabs.length}
              onOpenCollaborator={openCollaboratorMode}
              onCloseCollaborator={closeCollaboratorMode}
            />
          }
        />
        {/* Collaborator Mode pill — labelled so users immediately know what
            it does (the previous icon-only Users glyph was unreadable). The
            text + accent active-state mirrors the rest of the toolbar pills.
            Hidden until you're actually in a terminal session. */}
        {inSession && canShowCollaborator && terminalTabCount > 0 && (
          <button
            className={"collab-pill" + (collaboratorMode ? " active" : "")}
            onClick={() => {
              if (collaboratorMode) {
                closeCollaboratorMode();
              } else {
                openCollaboratorMode();
              }
            }}
            title={collaboratorMode ? "Exit Collaborator Mode" : "Open Collaborator Mode (side-by-side terminals)"}
            aria-pressed={collaboratorMode}
          >
            <span>Collaborator Mode</span>
            {terminalTabCount > 1 && <span className="collab-pill-count">{terminalTabCount}</span>}
          </button>
        )}
        {inSession && activeIsTerminal && (
          <button
            className={"topbar-drawer-toggle" + (drawerOpen ? " active" : "")}
            onClick={() => setDrawerOpen(o => !o)}
            title={collaboratorMode ? (drawerOpen ? "Hide themes" : "Show themes") : (drawerOpen ? "Hide drawer" : "Show drawer")}
            aria-label={collaboratorMode ? (drawerOpen ? "Hide themes" : "Show themes") : (drawerOpen ? "Hide drawer" : "Show drawer")}
            aria-pressed={drawerOpen}
          >
            {collaboratorMode ? <Palette size={15} strokeWidth={1.75} /> : <PanelRight size={15} strokeWidth={1.75} />}
          </button>
        )}
      </header>

      <div className="body">

        {/* Sidebar only when NOT in a session. */}
        {!inSession && (
          <Sidebar
            active={section}
            onSelect={(s) => {
              if (s === "sftp") setSftpInitialHost(null);
              setSftpLauncherOpen(false);
              leaveSession();
              setSection(s);
            }}
            onAddHost={() => setAddDialogOpen(true)}
          />
        )}

        <main className="main">
          {!inSession && section === "hosts" && (
            <HostGrid
              hosts={filteredHosts}
              onSelect={openHostTab}
              onOpenSftp={openSftpFlow}
              onEdit={(host) => setEditingHost(host)}
              onDuplicate={duplicateHost}
              onDelete={deleteHost}
              onNew={() => setAddDialogOpen(true)}
              onOpenLocalTerminal={() => openLocalTerminal()}
            />
          )}
          {!inSession && section === "sftp" && (
            <SftpPage
              hosts={filteredHosts}
              initialHost={sftpInitialHost}
              onSelectHost={setSftpInitialHost}
              onOpenSession={openSftpFlow}
            />
          )}
          {!inSession && section === "keychain"    && <KeychainPage />}
          {!inSession && section === "snippets"    && (
            <SnippetsPage activeSessionId={snippetSessionId} />
          )}
          {!inSession && section === "tunnels"     && <TunnelsPage hosts={hosts} activeSessions={activeSessionsByHost} />}
          {!inSession && section === "known-hosts" && <KnownHostsPage hosts={hosts} />}
          {inCollab && (
            <Collaborator
              tabs={collaboratorTabs}
              activeTabId={activeTabId}
              appearance={appearance}
              roots={collaboratorWorkspaceRoots}
              layoutKey={`${drawerOpen}:${appearance.themeId}:${appearance.fontId}:${appearance.fontSize}`}
              explorerOpen={collaboratorExplorerOpen}
              onExplorerOpenChange={setCollaboratorExplorerOpen}
              fileExpanded={collaboratorFileExpanded}
              onFileExpandedChange={setCollaboratorFileExpanded}
              onActivate={setActiveTabId}
              onClose={closeTab}
              onOpenLocalTerminal={openCollaboratorTerminal}
              onOpenFolderTerminal={() => { void openFolderTerminal({ collaborator: true }); }}
            />
          )}
          {!inCollab && inSession && activeTab && activeTab.state.kind === "connecting" && (
            <ConnectingView host={activeTab.host} />
          )}
          {!inCollab && inSession && activeTab && activeTab.state.kind === "auth_needed" && (
            <ConnectingView host={activeTab.host} authPrompt={activeTab.state.reason}
              onCancel={() => closeTab(activeTab.id)}
              onConnect={async (secret, rememberKeyPassphrase) => {
                updateTab(activeTab.id, { state: { kind: "connecting" } });
                try {
                  const resp = await api.connect({
                    host_id: activeTab.host.id,
                    auth_secret: secret || null,
                    cols: 100,
                    rows: 30,
                    remember_key_passphrase: rememberKeyPassphrase,
                  });
                  if (!tabsRef.current.some(t => t.id === activeTab.id)) {
                    api.disconnect(resp.session_id).catch(() => {});
                    return;
                  }
                  if ((activeTab.kind ?? "ssh") === "ssh") {
                    ensureDisconnectListener(resp.session_id);
                  }
                  updateTab(activeTab.id, { state: { kind: "connected", sessionId: resp.session_id } });
                  if ((activeTab.kind ?? "ssh") === "ssh") {
                    refreshDetectedOs(activeTab.host, resp.session_id);
                  }
	                } catch (e) {
	                  const message = String(e);
	                  const needsSecret = message.toLowerCase().includes("password required") ||
	                    message.toLowerCase().includes("passphrase") ||
	                    message.toLowerCase().includes("auth");
	                  updateTab(activeTab.id, {
	                    state: needsSecret
	                      ? {
	                          kind: "auth_needed",
	                          reason: activeTab.host.auth_kind === "password"
	                            ? "Password failed. Enter password"
	                            : "Passphrase failed. Enter key passphrase",
	                        }
	                      : { kind: "error", message },
	                  });
	                }
	              }} />
          )}
          {!inCollab && inSession && activeTab && activeTab.state.kind === "reconnecting" && (
            <ConnectingView
              host={activeTab.host}
              reconnect={{
                attempt: activeTab.state.attempt,
                maxAttempts: RECONNECT_DELAYS_MS.length,
                nextRetryAt: activeTab.state.nextRetryAt,
              }}
              onCancel={() => closeTab(activeTab.id)}
              onRetry={() => {
                // Restart series at attempt 1 with 0 delay
                const tabId = activeTab.id;
                const host = activeTab.host;
                const series = (reconnectAttempt.current.get(tabId) ?? 0) + 1;
                reconnectAttempt.current.set(tabId, series);
                updateTab(tabId, {
                  state: { kind: "reconnecting", attempt: 1, nextRetryAt: Date.now(), lastSessionId: "" },
                });
                attemptReconnect(tabId, host, series, 1);
              }}
            />
          )}
          {!inCollab && inSession && activeTab && activeTab.state.kind === "error" && (
            <ConnectingView host={activeTab.host} error={activeTab.state.message}
              onCancel={() => closeTab(activeTab.id)}
              onRetry={() => {
                if (activeTab.kind === "local") {
                  void retryLocalTab(activeTab.id, typeof activeTab.localStartCwd === "string" ? activeTab.localStartCwd : undefined);
                } else if (activeTab.kind === "sftp") {
                  void retrySftpTab(activeTab.id, activeTab.host);
                } else {
                  void retrySshTab(activeTab.id, activeTab.host);
                }
              }} />
          )}
          {!inCollab && inSession && persistentSessionTabs.map(tab => {
            const isActive = tab.id === activeTabId;
            const tabAppearance = appearanceForTab(tab);
            const paneStyle = themeVarStyle(tabAppearance.themeId) as CSSProperties;
            if (tab.kind === "sftp" && tab.state.kind === "connected") {
              return (
                <div
                  key={tab.id}
                  className={"session-pane" + (isActive ? " active" : "")}
                  style={paneStyle}
                  aria-hidden={!isActive}
                >
                  <SftpPage
                    hosts={filteredHosts}
                    sessionHost={tab.host}
                    sessionId={tab.state.sessionId}
                    initialPath={tab.sftpCwd ?? "~"}
                    onPathChange={(path) => updateTab(tab.id, { sftpCwd: path })}
                    onSelectHost={() => {}}
                  />
                </div>
              );
            }
            return (
              <div
                key={tab.id}
                className={"session-pane" + (isActive ? " active" : "")}
                style={paneStyle}
                aria-hidden={!isActive}
              >
                <TerminalView
                  tab={tab}
                  appearance={tabAppearance}
                  active={isActive}
                  onPendingChip={(chip) => updateTab(tab.id, { pendingChip: chip })}
                  initialOutput={replayForTab(tab.id)}
                  onOutputChunk={(chunk) => rememberTerminalOutput(tab.id, chunk)}
                  drawerOpen={drawerOpen}
                  allowUploads={tab.kind !== "local"}
                  onFocusRequest={() => setActiveTabId(tab.id)}
                />
              </div>
            );
          })}
        </main>

        {inSession && activeIsTerminal && drawerOpen && (
          <Drawer
            variant={collaboratorMode ? "collaborator-theme" : "full"}
            appearance={appearance}
            onChange={updateAppearance}
            activeSessionId={activeTab?.state.kind === "connected" ? activeTab.state.sessionId : null}
            activeHost={activeTab?.state.kind === "connected" ? activeTab.host : null}
            hosts={hosts}
            projectScope={projectScope}
            liveSessionIds={liveSessionIds}
          />
        )}
      </div>

      {sftpLauncherOpen && (
        <div
          className="sftp-overlay sftp-launcher-overlay"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) setSftpLauncherOpen(false);
          }}
        >
          <section className="sftp-panel sftp-launcher-panel" aria-label="Open SFTP session">
            <header className="sftp-header">
              <div className="sftp-title">
                <Folder size={16} />
                <span>Open SFTP</span>
              </div>
              <button className="icon-tool" title="Close" aria-label="Close" onClick={() => setSftpLauncherOpen(false)}>
                <X size={15} />
              </button>
            </header>
            <div className="sftp-launcher-list">
              {filteredHosts.length === 0 ? (
                <div className="sftp-empty-state">No saved hosts yet.</div>
              ) : filteredHosts.map(host => (
                <button
                  key={host.id}
                  type="button"
                  className="sftp-host-row"
                  title="Open SFTP"
                  onClick={() => openSftpFlow(host)}
                >
                  <OsBadge os={host.os ?? "linux"} size={32} />
                  <div>
                    <strong>{host.label}</strong>
                    <span>{host.username}@{host.hostname}{host.port !== 22 ? `:${host.port}` : ""}</span>
                  </div>
                </button>
              ))}
            </div>
          </section>
        </div>
      )}

      {(addDialogOpen || editingHost) && (
        <HostInspector
          hosts={hosts}
          initialHost={editingHost}
          onClose={() => { setAddDialogOpen(false); setEditingHost(null); }}
          onSaved={async (host, password) => {
            setAddDialogOpen(false);
            setEditingHost(null);
            await refreshHosts();
            if (host && !editingHost) connectHost(host, password ?? null, !!password);
          }}
          onSavedFallback={(host) => {
            const next = editingHost
              ? hosts.map(h => h.id === host.id ? host : h)
              : [...hosts, host];
            saveFallbackHosts(next);
            setHosts(next);
            setAddDialogOpen(false);
            setEditingHost(null);
          }}
        />
      )}

      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        hosts={hosts}
        onSelectHost={openHostTab}
        onOpenSftp={openSftpFlow}
        onSelectSection={(s) => {
          if (s === "sftp") setSftpInitialHost(null);
          setSftpLauncherOpen(false);
          leaveSession();
          setSection(s);
        }}
        onAddHost={() => setAddDialogOpen(true)}
        appearance={appearance}
        onAppearance={updateAppearance}
      />

    </div>
  );
}
