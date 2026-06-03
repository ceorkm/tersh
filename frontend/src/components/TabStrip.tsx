import { X, Plus, Folder, LayoutGrid, SquareTerminal } from "lucide-react";
import { OsGlyph } from "../assets/os-icons";
import { VaultGlyph } from "./VaultGlyph";
import type { Tab } from "../types";

interface Props {
  tabs: Tab[];
  activeTabId: string | null;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onNewTab: () => void;
  onOpenVaults: () => void;
  onOpenSftp: () => void;
  sftpEnabled: boolean;
  /** When true, the strip hides individual terminal tabs (ssh + local) and
   * shows a single "Collaborator · N" chip instead. Adding 6 terminals
   * shouldn't flood the top bar — they all live in the grid. */
  collaboratorMode?: boolean;
  collaboratorCount?: number;
  onOpenCollaborator?: () => void;
  onCloseCollaborator?: () => void;
}

export function TabStrip({
  tabs, activeTabId, onActivate, onClose, onNewTab,
  onOpenVaults, onOpenSftp, sftpEnabled,
  collaboratorMode = false, collaboratorCount = 0, onOpenCollaborator, onCloseCollaborator,
}: Props) {
  return (
    <div className="tabstrip">
      <button className="tab-fixed" title="Vaults" onClick={onOpenVaults}>
        <VaultGlyph size={13} /> Vaults
      </button>
      <button
        className="tab-fixed"
        title={sftpEnabled ? "Open SFTP for this host" : "Open SFTP from the sidebar"}
        onClick={onOpenSftp}
        disabled={!sftpEnabled}
        style={!sftpEnabled ? { opacity: 0.5, cursor: "not-allowed" } : {}}
      >
        <Folder size={13} strokeWidth={1.75} /> SFTP
      </button>
      <div className="tab-sep" />

      {collaboratorCount > 0 && (
        <button
          className={"tab tab-collab" + (collaboratorMode ? " active" : "")}
          onClick={onOpenCollaborator}
          title={collaboratorMode ? "Collaborator mode" : "Open Collaborator mode"}
          aria-pressed={collaboratorMode}
        >
          <span className="tab-os">
            <LayoutGrid size={12} strokeWidth={1.75} />
          </span>
          <span className="label">Collaborator · {collaboratorCount}</span>
          <span
            className="close"
            onClick={e => {
              e.stopPropagation();
              onCloseCollaborator?.();
            }}
            role="button"
            aria-label="Close Collaborator"
          >
            <X size={12} strokeWidth={2.25} />
          </span>
        </button>
      )}

      {tabs.map(t => {
        const isActive = t.id === activeTabId;
        const isConnecting = t.state.kind === "connecting";
        const isAuthOrError = t.state.kind === "auth_needed" || t.state.kind === "error";
        const isLocal = t.kind === "local";
        const isSftp = t.kind === "sftp";
        const localCwd = typeof t.localCwd === "string" ? t.localCwd : null;
        return (
          <button
            key={t.id}
            className={"tab" + (isActive ? " active" : "") + (isConnecting ? " connecting" : "")}
            onClick={() => onActivate(t.id)}
            title={isSftp ? `SFTP · ${t.host.username}@${t.host.hostname}` : isLocal ? (localCwd ?? "Local terminal") : `${t.host.username}@${t.host.hostname}`}
          >
            <span className="tab-os">
              {isSftp
                ? <Folder size={13} strokeWidth={1.75} />
                : isLocal
                ? <SquareTerminal size={13} strokeWidth={1.75} />
                : <OsGlyph os={t.host.os ?? "linux"} size={13} />}
            </span>
            <span className="label">{tabDisplayLabel(t, tabs)}</span>
            {isConnecting ? (
              <span className="pulse" aria-hidden />
            ) : (
              <span
                className="close"
                onClick={e => { e.stopPropagation(); onClose(t.id); }}
                role="button"
                aria-label="Close tab"
              >
                <X size={12} strokeWidth={2.25} />
              </span>
            )}
            {isAuthOrError && <span className="state-dot" aria-hidden />}
          </button>
        );
      })}

      <button className="new-tab-btn" onClick={onNewTab} title="New host / connection" aria-label="New host or connection">
        <Plus size={14} strokeWidth={2} />
      </button>
    </div>
  );
}

function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.split("/").filter(Boolean).pop() || path;
}

function tabDisplayLabel(tab: Tab, tabs: Tab[]): string {
  if (tab.kind === "sftp") return `SFTP · ${tab.host.label}`;
  if (tab.kind === "local" && typeof tab.localCwd === "string") return basename(tab.localCwd);

  const sameHostTabs = tabs.filter(t => (t.kind ?? "ssh") === "ssh" && t.host.id === tab.host.id);
  if (sameHostTabs.length <= 1) return tab.host.label;

  const index = sameHostTabs.findIndex(t => t.id === tab.id);
  return index <= 0 ? tab.host.label : `${tab.host.label} (${index})`;
}
