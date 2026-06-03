import {
  ArrowDownAZ, ArrowUpAZ, CalendarArrowDown, CalendarArrowUp,
  ChevronDown, Copy, Edit3, Folder, LayoutGrid, List,
  Plus, ShieldCheck, SquareTerminal, Trash2,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { OsBadge } from "../assets/os-icons";
import type { HostRow } from "../types";

type ViewMode = "grid" | "list";
type SortMode = "az" | "za" | "newest" | "oldest";

const VIEW_KEY = "tersh:hosts:view";
const SORT_KEY = "tersh:hosts:sort";

const loadView = (): ViewMode => {
  try {
    const v = localStorage.getItem(VIEW_KEY);
    return v === "list" ? "list" : "grid";
  } catch { return "grid"; }
};
const loadSort = (): SortMode => {
  try {
    const v = localStorage.getItem(SORT_KEY);
    if (v === "az" || v === "za" || v === "newest" || v === "oldest") return v;
  } catch {}
  return "az";
};

interface Props {
  hosts: HostRow[];
  onSelect: (h: HostRow) => void;
  onOpenSftp: (h: HostRow) => void;
  onEdit: (h: HostRow) => void;
  onDuplicate: (h: HostRow) => void;
  onDelete: (id: string) => void;
  onNew: () => void;
  onOpenLocalTerminal: () => void;
}

export function HostGrid({ hosts, onSelect, onOpenSftp, onEdit, onDuplicate, onDelete, onNew, onOpenLocalTerminal }: Props) {
  const [menu, setMenu] = useState<{ host: HostRow; x: number; y: number } | null>(null);
  const [view, setView] = useState<ViewMode>(loadView);
  const [sort, setSort] = useState<SortMode>(loadSort);

  useEffect(() => { try { localStorage.setItem(VIEW_KEY, view); } catch {} }, [view]);
  useEffect(() => { try { localStorage.setItem(SORT_KEY, sort); } catch {} }, [sort]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") close(); };
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  const copyHostInfo = async (host: HostRow) => {
    const port = host.port !== 22 ? ` -p ${host.port}` : "";
    await navigator.clipboard.writeText(`ssh ${host.username}@${host.hostname}${port}`);
  };

  // Sort INSIDE each group; backend returns hosts in insertion order so
  // 'oldest' is array order and 'newest' is its reverse. No created_at on
  // HostRow yet — when that lands, swap the index sort for a timestamp.
  const sortHosts = useMemo(() => (items: HostRow[]): HostRow[] => {
    const arr = items.slice();
    switch (sort) {
      case "az":     return arr.sort((a, b) => a.label.localeCompare(b.label, undefined, { sensitivity: "base" }));
      case "za":     return arr.sort((a, b) => b.label.localeCompare(a.label, undefined, { sensitivity: "base" }));
      case "newest": return arr.reverse();
      case "oldest": return arr;
    }
  }, [sort]);

  if (hosts.length === 0) {
    return (
      <div className="empty">
        <div className="empty-icon">
          <SquareTerminal size={48} strokeWidth={1.5} />
        </div>
        <h3>No hosts yet</h3>
        <p>Add your first SSH host to get started.</p>
        <div className="empty-actions">
          <button className="primary" onClick={onNew}>
            <Plus size={14} /> New host
          </button>
          <button className="toolbar-btn" onClick={onOpenLocalTerminal}>
            <SquareTerminal size={14} /> Terminal
          </button>
        </div>
      </div>
    );
  }

  // group by group_name
  const groups = new Map<string, HostRow[]>();
  for (const h of hosts) {
    const k = h.group_name ?? "Ungrouped";
    const list = groups.get(k) ?? [];
    list.push(h);
    groups.set(k, list);
  }
  // Suppress the section header when everything's in one default "Ungrouped"
  // bucket. Only show the heading once the user actually organises hosts into
  // named groups.
  const groupKeys = [...groups.keys()];
  const hideHeader = groupKeys.length === 1 && groupKeys[0] === "Ungrouped";

  return (
    <>
      <Toolbar
        onNew={onNew}
        onOpenLocalTerminal={onOpenLocalTerminal}
        view={view}
        onViewChange={setView}
        sort={sort}
        onSortChange={setSort}
      />
      <div className="content-scroll">
        {[...groups.entries()].map(([group, items]) => (
          <section className="host-section" key={group}>
            {!hideHeader && (
              <header className="section-header">
                <h2>{group}</h2>
                <span className="count">{items.length}</span>
              </header>
            )}
            <div className={view === "grid" ? "host-grid" : "host-list"}>
              {sortHosts(items).map(h => {
                // If the user never set a label, label falls back to hostname
                // (see backend normalize_host_input). In that case repeating
                // the IP in the subtitle makes the card look duplicated —
                // show only `ssh · user`. When label IS distinct, show the
                // full address so the user knows where it points.
                const port = h.port !== 22 ? `:${h.port}` : "";
                const labelIsAddress = h.label === h.hostname || h.label === `${h.hostname}${port}`;
                return (
                <div
                  className="host-card"
                  key={h.id}
                  onClick={() => onSelect(h)}
                  onContextMenu={e => {
                    e.preventDefault();
                    setMenu({ host: h, x: e.clientX, y: e.clientY });
                  }}
                  role="button"
                  tabIndex={0}
                  onKeyDown={e => { if (e.key === "Enter") onSelect(h); }}
                >
                  <OsBadge os={h.os ?? "linux"} size={32} />
                  <div className="meta">
                    <div className="label">{h.label}</div>
                    <div className="sub">
                      <span className="proto">ssh,</span>
                      <span className="user">{h.username}</span>
                      {!labelIsAddress && (
                        <>
                          <span className="dot-sep">·</span>
                          <span className="addr">{h.hostname}{port}</span>
                        </>
                      )}
                    </div>
                  </div>
                  <button
                    className="card-edit"
                    onClick={e => {
                      e.stopPropagation();
                      onEdit(h);
                    }}
                    aria-label="Edit host"
                  >
                    <Edit3 size={14} strokeWidth={2} />
                  </button>
                </div>
                );
              })}
            </div>
          </section>
        ))}
      </div>
      {menu && (
        <div
          className="host-context-menu"
          style={{ left: menu.x, top: menu.y }}
          onClick={e => e.stopPropagation()}
        >
          <button className="host-context-item" onClick={() => { onSelect(menu.host); setMenu(null); }}>
            <SquareTerminal size={14} /> Quick Connect
          </button>
          <button className="host-context-item" onClick={() => { onSelect(menu.host); setMenu(null); }}>
            <SquareTerminal size={14} /> Connect via SSH
          </button>
          <button className="host-context-item" onClick={() => { onOpenSftp(menu.host); setMenu(null); }}>
            <Folder size={14} /> Open SFTP
          </button>
          <button className="host-context-item" onClick={() => { onEdit(menu.host); setMenu(null); }}>
            <Edit3 size={14} /> Edit Host Details
          </button>
          <button className="host-context-item" onClick={() => { onDuplicate(menu.host); setMenu(null); }}>
            <Copy size={14} /> Duplicate Host
          </button>
          <button className="host-context-item" onClick={() => { void copyHostInfo(menu.host); setMenu(null); }}>
            <Copy size={14} /> Copy Host Info
          </button>
          <div className="host-context-sep" />
          <button
            className="host-context-item danger"
            onClick={() => {
              if (confirm(`Remove "${menu.host.label}"?`)) onDelete(menu.host.id);
              setMenu(null);
            }}
          >
            <Trash2 size={14} /> Remove
          </button>
        </div>
      )}
    </>
  );
}

interface ToolbarProps {
  onNew: () => void;
  onOpenLocalTerminal: () => void;
  view: ViewMode;
  onViewChange: (v: ViewMode) => void;
  sort: SortMode;
  onSortChange: (s: SortMode) => void;
}

function Toolbar({ onNew, onOpenLocalTerminal, view, onViewChange, sort, onSortChange }: ToolbarProps) {
  const [viewOpen, setViewOpen] = useState(false);
  const [sortOpen, setSortOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);

  // Close any open menu on outside click / Escape. (Import is a modal, not
  // a menu — it manages its own close, so it isn't in this list.)
  useEffect(() => {
    if (!viewOpen && !sortOpen) return;
    const close = () => { setViewOpen(false); setSortOpen(false); };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") close(); };
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [viewOpen, sortOpen]);

  const stop = (e: React.MouseEvent) => e.stopPropagation();

  return (
    <div className="main-toolbar" onClick={stop}>
      {/* Split button — main = open New Host form; chevron = open the
          import modal. */}
      <div className="split-btn">
        <button className="toolbar-btn primary-ghost split-btn-main" onClick={onNew}>
          <Plus size={14} strokeWidth={2} /> New host
        </button>
        <button
          className="toolbar-btn primary-ghost split-btn-chev"
          aria-label="Import hosts"
          onClick={() => setImportOpen(true)}
        >
          <ChevronDown size={13} strokeWidth={2} />
        </button>
      </div>
      {importOpen && <ImportHostsModal onClose={() => setImportOpen(false)} />}

      <button className="toolbar-btn" onClick={onOpenLocalTerminal}>
        <SquareTerminal size={14} strokeWidth={2} /> Terminal
      </button>

      <div className="grow" />

      {/* View toggle (grid / list) */}
      <div className="split-btn">
        <button
          className={"icon-tool" + (viewOpen ? " active" : "")}
          aria-label="Change view"
          aria-haspopup="menu"
          aria-expanded={viewOpen}
          title="View"
          onClick={() => { setViewOpen(o => !o); setSortOpen(false); }}
        >
          {view === "grid" ? <LayoutGrid size={15} strokeWidth={2} /> : <List size={15} strokeWidth={2} />}
        </button>
        {viewOpen && (
          <div className="toolbar-menu align-right" role="menu" onClick={stop}>
            <button
              className={"toolbar-menu-item" + (view === "grid" ? " active" : "")}
              role="menuitem"
              onClick={() => { onViewChange("grid"); setViewOpen(false); }}
            >
              <LayoutGrid size={13} strokeWidth={2} /> Grid
            </button>
            <button
              className={"toolbar-menu-item" + (view === "list" ? " active" : "")}
              role="menuitem"
              onClick={() => { onViewChange("list"); setViewOpen(false); }}
            >
              <List size={13} strokeWidth={2} /> List
            </button>
          </div>
        )}
      </div>

      {/* Sort dropdown */}
      <div className="split-btn">
        <button
          className={"icon-tool" + (sortOpen ? " active" : "")}
          aria-label="Sort"
          aria-haspopup="menu"
          aria-expanded={sortOpen}
          title="Sort"
          onClick={() => { setSortOpen(o => !o); setViewOpen(false); }}
        >
          {sort === "az"     ? <ArrowDownAZ      size={15} strokeWidth={2} /> :
           sort === "za"     ? <ArrowUpAZ        size={15} strokeWidth={2} /> :
           sort === "newest" ? <CalendarArrowDown size={15} strokeWidth={2} /> :
                               <CalendarArrowUp  size={15} strokeWidth={2} />}
        </button>
        {sortOpen && (
          <div className="toolbar-menu align-right" role="menu" onClick={stop}>
            <button
              className={"toolbar-menu-item" + (sort === "az" ? " active" : "")}
              role="menuitem"
              onClick={() => { onSortChange("az"); setSortOpen(false); }}
            >
              <ArrowDownAZ size={13} strokeWidth={2} /> A → Z
            </button>
            <button
              className={"toolbar-menu-item" + (sort === "za" ? " active" : "")}
              role="menuitem"
              onClick={() => { onSortChange("za"); setSortOpen(false); }}
            >
              <ArrowUpAZ size={13} strokeWidth={2} /> Z → A
            </button>
            <button
              className={"toolbar-menu-item" + (sort === "newest" ? " active" : "")}
              role="menuitem"
              onClick={() => { onSortChange("newest"); setSortOpen(false); }}
            >
              <CalendarArrowDown size={13} strokeWidth={2} /> Newest first
            </button>
            <button
              className={"toolbar-menu-item" + (sort === "oldest" ? " active" : "")}
              role="menuitem"
              onClick={() => { onSortChange("oldest"); setSortOpen(false); }}
            >
              <CalendarArrowUp size={13} strokeWidth={2} /> Oldest first
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// Import modal. Full-screen overlay, vault icon at top, title + subtitle,
// then a row of source tiles. Each tile stubs to "Coming soon" until a
// backend parser for that source is wired up.
interface ImportSource {
  id: string;
  label: string;
  mark: React.ReactNode;
}

function ImportHostsModal({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const pick = (label: string) => () => {
    alert(`${label} import: coming soon.\nFor now use "+ New host".`);
    onClose();
  };

  const sources: ImportSource[] = [
    { id: "ssh",   label: "~/.ssh",    mark: <SourceMarkSsh /> },
    { id: "csv",   label: "CSV",       mark: <SourceMarkCsv /> },
  ];

  return (
    <div className="import-modal-backdrop" onClick={onClose}>
      <div className="import-modal" role="dialog" aria-modal="true" aria-labelledby="import-modal-title" onClick={e => e.stopPropagation()}>
        <div className="import-modal-hero" aria-hidden="true">
          <ShieldCheck size={40} strokeWidth={1.5} />
        </div>
        <h2 id="import-modal-title" className="import-modal-title">Add data to your vault</h2>
        <p className="import-modal-sub">
          Transfer your connections, SSH keys, known hosts, and port forwarding to Tersh.
          Select a file format to start the migration.
        </p>
        <div className="import-modal-tiles">
          {sources.map(s => (
            <button key={s.id} className="import-tile" onClick={pick(s.label)} aria-label={`Import from ${s.label}`}>
              <span className="import-tile-mark">{s.mark}</span>
              <span className="import-tile-label">{s.label}</span>
            </button>
          ))}
        </div>
        <footer className="import-modal-foot">
          <button className="import-modal-later" onClick={onClose}>Later</button>
          <span className="import-modal-help">
            Need to import something else?{" "}
            <a href="#" onClick={e => { e.preventDefault(); alert("Let us know on the project issue tracker."); }}>Let us know</a>
          </span>
        </footer>
      </div>
    </div>
  );
}

// Tiny inline SVG marks — flat, themed (no full-color brand logos so the
// modal doesn't read as a marketing page). Each mark uses currentColor
// so the active/hover state can tint with the theme accent.
function SourceMarkSsh() {
  return (
    <svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h4l2 2h7A2.5 2.5 0 0 1 21 9.5V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      <text x="7" y="16" fontSize="6" fontWeight="700" fill="currentColor" stroke="none" fontFamily="ui-monospace, monospace">SSH</text>
    </svg>
  );
}
function SourceMarkCsv() {
  return (
    <svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
      <path d="M14 3v5h5" />
      <text x="7.5" y="17.5" fontSize="5" fontWeight="700" fill="currentColor" stroke="none" fontFamily="ui-monospace, monospace">CSV</text>
    </svg>
  );
}
