import { Edit3, Folder, Server, SquareTerminal, Trash2 } from "lucide-react";
import { OsBadge } from "../assets/os-icons";
import type { HostRow } from "../types";

interface Props {
  hosts: HostRow[];
  onClose: () => void;
  onConnectSsh: (host: HostRow) => void;
  onOpenSftp: (host: HostRow) => void;
  onEditHost: (host: HostRow) => void;
  onDeleteHost: (id: string) => void;
}

export function VaultsMenu({
  hosts,
  onClose,
  onConnectSsh,
  onOpenSftp,
  onEditHost,
  onDeleteHost,
}: Props) {
  const run = (fn: () => void) => {
    fn();
    onClose();
  };

  return (
    <>
      <div className="popover-scrim" onClick={onClose} />
      <div className="popover vaults-menu">
        <div className="popover-section">
          <div className="popover-title">Saved Connections</div>
          {hosts.length === 0 ? (
            <div className="vault-empty">
              <Server size={18} />
              <span>No saved hosts yet.</span>
            </div>
          ) : (
            <div className="vault-host-list">
              {hosts.map(host => (
                <div className="vault-host-row" key={host.id}>
                  <OsBadge os={host.os ?? "linux"} size={28} />
                  <div className="vault-host-meta">
                    <strong>{host.label}</strong>
                    <span>{host.username}@{host.hostname}{host.port !== 22 ? `:${host.port}` : ""}</span>
                  </div>
                  <button title="Connect via SSH" aria-label={`Connect to ${host.label} via SSH`} onClick={() => run(() => onConnectSsh(host))}>
                    <SquareTerminal size={14} />
                  </button>
                  <button title="Open SFTP" aria-label={`Open ${host.label} with SFTP`} onClick={() => run(() => onOpenSftp(host))}>
                    <Folder size={14} />
                  </button>
                  <button title="Edit host" aria-label={`Edit ${host.label}`} onClick={() => run(() => onEditHost(host))}>
                    <Edit3 size={14} />
                  </button>
                  <button
                    className="danger"
                    title="Remove host"
                    aria-label={`Remove ${host.label}`}
                    onClick={() => {
                      if (confirm(`Remove "${host.label}"?`)) run(() => onDeleteHost(host.id));
                    }}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </>
  );
}
