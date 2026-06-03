import { Search } from "lucide-react";
import { type ReactNode } from "react";

interface Props {
  search: string;
  onSearch: (s: string) => void;
  showSearch: boolean;
  tabStrip: ReactNode;
}

export function TopBar({ search, onSearch, showSearch, tabStrip }: Props) {
  return (
    <>
      {tabStrip}

      {showSearch && (
        <div className="topbar-search">
          <Search size={14} strokeWidth={2} />
          <input
            placeholder="Find a host"
            value={search}
            onChange={e => onSearch(e.target.value)}
            spellCheck={false}
          />
          <kbd className="kbd">⌘K</kbd>
        </div>
      )}
    </>
  );
}
