import { AlertCircle, ArrowRight, FolderOpen, GitBranch, PanelLeftClose, PanelLeftOpen, Plus, RefreshCw, Sparkles, SquareTerminal, X, Loader2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { OsGlyph } from "../assets/os-icons";
import type { Tab } from "../types";
import { CollaboratorFileTree } from "./CollaboratorFileTree";
import { CollaboratorWebviewTile } from "./CollaboratorWebviewTile";
import type { Appearance } from "../lib/appearance";

const COLLAB_INTRO_KEY = "tersh:collaborator-intro-seen";

interface Props {
  tabs: Tab[];
  activeTabId: string | null;
  appearance: Appearance;
  roots: string[];
  layoutKey: string;
  explorerOpen: boolean;
  onExplorerOpenChange: (open: boolean) => void;
  fileExpanded: Set<string>;
  onFileExpandedChange: (expanded: Set<string>) => void;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onOpenLocalTerminal: () => void;
  onOpenFolderTerminal: () => void;
}

export function Collaborator({
  tabs,
  activeTabId,
  appearance,
  roots,
  layoutKey,
  explorerOpen,
  onExplorerOpenChange,
  fileExpanded,
  onFileExpandedChange,
  onActivate,
  onClose,
  onOpenLocalTerminal,
  onOpenFolderTerminal,
}: Props) {
  const [showIntro, setShowIntro] = useState(() => {
    try {
      return localStorage.getItem(COLLAB_INTRO_KEY) !== "1";
    } catch {
      return true;
    }
  });
  const gridRef = useRef<HTMLDivElement>(null);
  const terminalTabs = tabs.filter(t => (t.kind ?? "ssh") === "ssh" || t.kind === "local");
  const terminalAppearance: Appearance = {
    ...appearance,
    themeId: "flexoki-dark",
  };
  const workspaceRoots = Array.from(new Set([
    ...roots,
    ...terminalTabs
      .filter(t => t.kind === "local" && typeof t.localCwd === "string" && t.localCwd.trim())
      .map(t => t.localCwd as string),
  ]));

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    listen<{ deltaX: number; deltaY: number; shiftKey: boolean }>("collab://terminal-wheel", event => {
      const grid = gridRef.current;
      if (!grid) return;
      const deltaX = Number(event.payload?.deltaX) || 0;
      const deltaY = Number(event.payload?.deltaY) || 0;
      const shiftKey = Boolean(event.payload?.shiftKey);
      const horizontalIntent = Math.abs(deltaX) > Math.abs(deltaY) || shiftKey;
      if (!horizontalIntent) return;
      grid.scrollBy({
        left: deltaX || deltaY,
        top: 0,
        behavior: "auto",
      });
    }).then(fn => {
      if (cancelled) fn();
      else unlisten = fn;
    }).catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const enterCollaborator = () => {
    try {
      localStorage.setItem(COLLAB_INTRO_KEY, "1");
    } catch {}
    setShowIntro(false);
  };

  if (showIntro) {
    return (
      <div className="collab collab-intro">
        <section className="collab-intro-copy" aria-labelledby="collab-intro-title">
          <span className="collab-intro-kicker">
            <Sparkles size={14} strokeWidth={1.8} />
            Introducing Collaborator Mode
          </span>
          <h1 id="collab-intro-title">Build with every agent in view.</h1>
          <p>
            Collaborator turns your terminal into a shared workbench: multiple local terminals,
            folders, and agents arranged together so you can compare, steer, and ship without tab hunting.
          </p>
          <div className="collab-intro-points">
            <span><SquareTerminal size={15} strokeWidth={1.8} /> Independent terminals</span>
            <span><FolderOpen size={15} strokeWidth={1.8} /> Workspace-aware folders</span>
            <span><GitBranch size={15} strokeWidth={1.8} /> Parallel agent work</span>
          </div>
          <div className="collab-intro-actions">
            <button type="button" className="collab-action primary" onClick={enterCollaborator}>
              Enter Collaborator <ArrowRight size={14} strokeWidth={2} />
            </button>
            <button
              type="button"
              className="collab-action"
              onClick={() => {
                enterCollaborator();
                if (terminalTabs.length === 0) onOpenLocalTerminal();
              }}
            >
              <Plus size={14} strokeWidth={2} /> Start with terminal
            </button>
          </div>
        </section>
        <CollaboratorIllustration />
      </div>
    );
  }

  if (terminalTabs.length === 0) {
    return (
      <div className="collab empty-collab">
        <SquareTerminal size={32} strokeWidth={1.4} />
        <h2>Collaborator Mode</h2>
        <p>Open multiple terminals side-by-side. Add as many as you need.</p>
        <div className="collab-empty-actions">
          <button type="button" className="collab-action primary" onClick={onOpenLocalTerminal}>
            <Plus size={14} strokeWidth={2} /> New terminal
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className={"collab-workspace" + (explorerOpen ? "" : " explorer-closed")}>
      {explorerOpen ? (
        <div className="collab-file-shell">
          <CollaboratorFileTree
            roots={workspaceRoots}
            expanded={fileExpanded}
            onExpandedChange={onFileExpandedChange}
            onOpenFolder={onOpenFolderTerminal}
          />
          <button
            type="button"
            className="collab-panel-toggle"
            onClick={() => onExplorerOpenChange(false)}
            title="Hide explorer"
            aria-label="Hide explorer"
          >
            <PanelLeftClose size={15} strokeWidth={1.8} />
          </button>
        </div>
      ) : (
        <button
          type="button"
          className="collab-panel-restore"
          onClick={() => onExplorerOpenChange(true)}
          title="Show explorer"
          aria-label="Show explorer"
        >
          <PanelLeftOpen size={15} strokeWidth={1.8} />
        </button>
      )}
      <div className="collab">
        <header className="collab-head">
          <div>
            <span className="collab-eyebrow">Collaborator</span>
            <h1>{terminalTabs.length} terminal{terminalTabs.length === 1 ? "" : "s"}</h1>
          </div>
          <div className="collab-actions">
            <button type="button" className="collab-action primary" onClick={onOpenLocalTerminal} title="Add a terminal to the workspace">
              <Plus size={14} strokeWidth={2} /> Add terminal
            </button>
          </div>
        </header>

        <div ref={gridRef} className={"collab-grid count-" + Math.min(terminalTabs.length, 6)}>
          {terminalTabs.map(tab => {
            const active = tab.id === activeTabId;
            const connected = tab.state.kind === "connected";
            const isLocal = tab.kind === "local";
            return (
              <section
                key={tab.id}
                className={"collab-tile" + (active ? " active" : "")}
                onPointerDown={() => onActivate(tab.id)}
              >
                <header className="collab-tile-head">
                  <span className={"collab-host-icon" + (isLocal ? " local" : "")}>
                    {isLocal
                      ? <SquareTerminal size={14} strokeWidth={1.75} />
                      : <OsGlyph os={tab.host.os ?? "linux"} size={15} />}
                  </span>
                  <div className="collab-host-text">
                    <strong>{isLocal ? localTerminalName(tab) : tab.host.label}</strong>
                    <span>{isLocal ? localTerminalPath(tab) : `${tab.host.username}@${tab.host.hostname}`}</span>
                  </div>
                  <button
                    type="button"
                    className="collab-head-btn danger"
                    title="Close terminal"
                    aria-label="Close terminal"
                    onPointerDown={(e) => e.stopPropagation()}
                    onClick={(e) => {
                      e.stopPropagation();
                      onClose(tab.id);
                    }}
                  >
                    <X size={14} strokeWidth={2.2} />
                  </button>
                </header>

                <div className="collab-terminal">
                  {connected ? (
                    <CollaboratorWebviewTile
                      tab={tab}
                      active={active}
                      appearance={terminalAppearance}
                      layoutKey={`${layoutKey}:collab-terminal-black`}
                      onActivate={onActivate}
                    />
                  ) : (
                    <TileStateCard tab={tab} />
                  )}
                </div>
              </section>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function CollaboratorIllustration() {
  return (
    <div className="collab-illustration" aria-hidden="true">
      <div className="collab-illustration-orbit one" />
      <div className="collab-illustration-orbit two" />
      <div className="collab-illustration-tile tile-a">
        <span />
        <b />
        <i />
      </div>
      <div className="collab-illustration-tile tile-b">
        <span />
        <b />
        <i />
      </div>
      <div className="collab-illustration-tile tile-c">
        <span />
        <b />
        <i />
      </div>
      <div className="collab-illustration-center">
        <SquareTerminal size={25} strokeWidth={1.7} />
      </div>
      <svg className="collab-illustration-lines" viewBox="0 0 420 320" preserveAspectRatio="none">
        <path d="M210 160 C150 120 118 100 78 74" />
        <path d="M210 160 C284 126 310 108 352 86" />
        <path d="M210 160 C240 218 268 246 310 270" />
      </svg>
    </div>
  );
}

function localTerminalName(tab: Tab): string {
  if (typeof tab.localCwd !== "string" || !tab.localCwd) return "Terminal";
  const trimmed = tab.localCwd.replace(/\/+$/, "");
  return trimmed.split("/").filter(Boolean).pop() || "Terminal";
}

function localTerminalPath(tab: Tab): string {
  return typeof tab.localCwd === "string" && tab.localCwd ? tab.localCwd : "local shell";
}

// Tile-mode state card. Future: extract into a compact mode on ConnectingView (see CLAUDE.md §7).
function TileStateCard({ tab }: { tab: Tab }) {
  if (tab.state.kind === "connecting") {
    return (
      <div className="collab-state-card">
        <Loader2 size={16} strokeWidth={2.25} className="spin" />
        <strong>Connecting</strong>
        <span>{tab.host.hostname}</span>
      </div>
    );
  }

  if (tab.state.kind === "reconnecting") {
    return (
      <div className="collab-state-card">
        <RefreshCw className="spin" size={18} strokeWidth={1.8} />
        <strong>Reconnecting</strong>
        <span>Attempt {tab.state.attempt}</span>
      </div>
    );
  }

  if (tab.state.kind === "auth_needed") {
    return (
      <div className="collab-state-card">
        <AlertCircle size={18} strokeWidth={1.8} />
        <strong>Needs credentials</strong>
        <span>{tab.state.reason}</span>
      </div>
    );
  }

  if (tab.state.kind === "error") {
    return (
      <div className="collab-state-card error">
        <AlertCircle size={18} strokeWidth={1.8} />
        <strong>Connection failed</strong>
        <span>{tab.state.message}</span>
      </div>
    );
  }

  return (
    <div className="collab-state-card">
      <SquareTerminal size={18} strokeWidth={1.8} />
      <strong>Idle</strong>
      <span>Click this tile to focus.</span>
    </div>
  );
}
