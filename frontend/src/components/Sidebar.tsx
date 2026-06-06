import { LayoutGrid, Key, Cable, Code2, ShieldCheck, Folder } from "lucide-react";
import { api } from "../lib/api";
import type { SidebarSection } from "../types";

interface Props {
  active: SidebarSection;
  onSelect: (s: SidebarSection) => void;
  onAddHost: () => void;
}

const NAV: { id: SidebarSection; label: string; Icon: typeof LayoutGrid }[] = [
  { id: "hosts",       label: "Hosts",            Icon: LayoutGrid },
  { id: "sftp",        label: "SFTP",             Icon: Folder },
  { id: "keychain",    label: "Keychain",         Icon: Key },
  { id: "tunnels",     label: "Port Forwarding",  Icon: Cable },
  { id: "snippets",    label: "Snippets",         Icon: Code2 },
  { id: "known-hosts", label: "Known Hosts",      Icon: ShieldCheck },
];

export function Sidebar({ active, onSelect }: Props) {
  return (
    <aside className="sidebar">
      <nav>
        <ul className="nav-list">
          {NAV.map(({ id, label, Icon }) => (
            <li
              key={id}
              className={"nav-item" + (active === id ? " active" : "")}
              onClick={() => onSelect(id)}
              role="button"
              tabIndex={0}
              onKeyDown={e => { if (e.key === "Enter") onSelect(id); }}
            >
              <Icon size={16} strokeWidth={1.75} />
              <span>{label}</span>
            </li>
          ))}
        </ul>
      </nav>
      <div className="sidebar-footer">
        <div className="sidebar-release">
          <span className="sidebar-release-badge">Alpha</span>
        </div>
        <p className="sidebar-alpha-note">
          Early preview. You may hit bugs; please create an issue when something feels off.
        </p>
        <button
          type="button"
          className="sidebar-maker"
          title="Open ceorkm on X"
          onClick={() => { void api.openExternalUrl("https://x.com/ceorkm"); }}
        >
          <span>Made with</span>
          <span className="sidebar-heart" aria-hidden="true">♥</span>
          <span>by</span>
          <img src="/brand/ceorkm.jpg" alt="" />
          <span className="sidebar-maker-name">ceorkm</span>
        </button>
      </div>
    </aside>
  );
}
