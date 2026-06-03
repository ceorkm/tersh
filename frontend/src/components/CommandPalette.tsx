import { useEffect, useMemo, useRef, useState } from "react";
import { LayoutGrid, Key, Cable, Code2, ShieldCheck, Plus, Sun, Moon, Folder } from "lucide-react";
import { OsBadge } from "../assets/os-icons";
import { THEMES } from "../themes";
import type { HostRow, SidebarSection } from "../types";
import type { Appearance } from "../lib/appearance";

interface Props {
  open: boolean;
  onClose: () => void;
  hosts: HostRow[];
  onSelectHost: (h: HostRow) => void;
  onOpenSftp: (h?: HostRow | null) => void;
  onSelectSection: (s: SidebarSection) => void;
  onAddHost: () => void;
  appearance: Appearance;
  onAppearance: (a: Appearance) => void;
}

interface Action {
  id: string;
  label: string;
  hint?: string;
  icon: React.ReactNode;
  run: () => void;
  group: string;
}

export function CommandPalette({ open, onClose, hosts, onSelectHost, onOpenSftp, onSelectSection, onAddHost, appearance, onAppearance }: Props) {
  const [q, setQ] = useState("");
  const [highlight, setHighlight] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    setQ("");
    setHighlight(0);
    const timer = window.setTimeout(() => inputRef.current?.focus(), 20);
    return () => window.clearTimeout(timer);
  }, [open]);

  const actions: Action[] = useMemo(() => {
    const items: Action[] = [];

    items.push({
      id: "new-host", label: "New host", hint: "Add a server",
      icon: <Plus size={14} />, group: "Actions",
      run: () => { onClose(); onAddHost(); },
    });

    for (const h of hosts) {
      items.push({
        id: `host-ssh-${h.id}`,
        label: h.label,
        hint: `${h.username}@${h.hostname}${h.port !== 22 ? `:${h.port}` : ""}`,
        icon: <OsBadge os={h.os ?? "linux"} size={20} />,
        group: "SSH",
        run: () => { onClose(); onSelectHost(h); },
      });
      items.push({
        id: `host-sftp-${h.id}`,
        label: `Open SFTP: ${h.label}`,
        hint: `${h.username}@${h.hostname}${h.port !== 22 ? `:${h.port}` : ""}`,
        icon: <Folder size={14} />,
        group: "SFTP",
        run: () => { onClose(); onOpenSftp(h); },
      });
    }

    const sections: { id: SidebarSection; label: string; Icon: typeof LayoutGrid }[] = [
      { id: "hosts", label: "Go to Hosts", Icon: LayoutGrid },
      { id: "sftp", label: "Go to SFTP", Icon: Folder },
      { id: "keychain", label: "Go to Keychain", Icon: Key },
      { id: "tunnels", label: "Go to Port Forwarding", Icon: Cable },
      { id: "snippets", label: "Go to Snippets", Icon: Code2 },
      { id: "known-hosts", label: "Go to Known Hosts", Icon: ShieldCheck },
    ];
    for (const s of sections) {
      items.push({
        id: `nav-${s.id}`, label: s.label,
        icon: <s.Icon size={14} />, group: "Navigation",
        run: () => { onClose(); onSelectSection(s.id); },
      });
    }

    for (const t of THEMES) {
      items.push({
        id: `theme-${t.id}`,
        label: `Theme: ${t.name}`,
        hint: t.mode,
        icon: t.mode === "dark" ? <Moon size={14} /> : <Sun size={14} />,
        group: "Appearance",
        run: () => { onClose(); onAppearance({ ...appearance, themeId: t.id }); },
      });
    }
    if (!q.trim()) return items;
    const term = q.toLowerCase();
    return items.filter(a =>
      a.label.toLowerCase().includes(term) ||
      (a.hint ?? "").toLowerCase().includes(term) ||
      a.group.toLowerCase().includes(term),
    );
  }, [q, hosts, appearance, onAddHost, onAppearance, onClose, onOpenSftp, onSelectHost, onSelectSection]);

  useEffect(() => { setHighlight(0); }, [q]);
  useEffect(() => {
    setHighlight(current => Math.max(0, Math.min(actions.length - 1, current)));
  }, [actions.length]);

  // Keyboard nav
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { onClose(); return; }
      if (e.key === "ArrowDown") { e.preventDefault(); setHighlight(h => Math.min(actions.length - 1, h + 1)); }
      if (e.key === "ArrowUp")   { e.preventDefault(); setHighlight(h => Math.max(0, h - 1)); }
      if (e.key === "Enter")     { e.preventDefault(); actions[highlight]?.run(); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, actions, highlight, onClose]);

  if (!open) return null;

  // group results by group
  const grouped = new Map<string, { action: Action; idx: number }[]>();
  actions.forEach((a, idx) => {
    const list = grouped.get(a.group) ?? [];
    list.push({ action: a, idx });
    grouped.set(a.group, list);
  });

  return (
    <div className="palette-scrim" onClick={onClose}>
      <div className="palette" onClick={e => e.stopPropagation()}>
        <input
          ref={inputRef}
          className="palette-input"
          placeholder="Type a host, action, or theme…"
          value={q}
          onChange={e => setQ(e.target.value)}
        />
        <div className="palette-results">
          {actions.length === 0 ? (
            <div className="palette-empty">No matches.</div>
          ) : (
            [...grouped.entries()].map(([group, items]) => (
              <div key={group} className="palette-group">
                <div className="palette-group-title">{group}</div>
                {items.map(({ action, idx }) => (
                  <button
                    key={action.id}
                    className={"palette-item" + (idx === highlight ? " active" : "")}
                    onMouseEnter={() => setHighlight(idx)}
                    onClick={() => action.run()}
                  >
                    <span className="palette-icon">{action.icon}</span>
                    <span className="palette-label">{action.label}</span>
                    {action.hint && <span className="palette-hint">{action.hint}</span>}
                  </button>
                ))}
              </div>
            ))
          )}
        </div>
        <div className="palette-footer">
          <kbd>↑↓</kbd> navigate <kbd>Enter</kbd> select <kbd>Esc</kbd> close
        </div>
      </div>
    </div>
  );
}
