import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, File, Folder, FolderOpen, Plus, Loader2 } from "lucide-react";
import { api } from "../lib/api";
import type { RemoteEntry } from "../types";

interface Props {
  roots: string[];
  expanded: Set<string>;
  onExpandedChange: (expanded: Set<string>) => void;
  onOpenFolder: () => void;
}

interface LoadedDir {
  loading: boolean;
  error: string | null;
  truncated: boolean;
  entries: RemoteEntry[];
}

function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.split("/").filter(Boolean).pop() || trimmed || "/";
}

function dirname(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const parts = trimmed.split("/").filter(Boolean);
  if (parts.length <= 1) return "/";
  return "/" + parts.slice(0, -1).join("/");
}

function sortEntries(entries: RemoteEntry[]): RemoteEntry[] {
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
  });
}

export function CollaboratorFileTree({ roots, expanded, onExpandedChange, onOpenFolder }: Props) {
  const [loaded, setLoaded] = useState<Record<string, LoadedDir>>({});
  const normalizedRoots = useMemo(
    () => Array.from(new Set(roots.filter(Boolean))).sort((a, b) => a.localeCompare(b)),
    [roots],
  );

  const setExpanded = (updater: (expanded: Set<string>) => Set<string>) => {
    onExpandedChange(updater(expanded));
  };

  const ensureLoaded = (path: string) => {
    const current = loaded[path];
    if (current?.loading || current?.entries.length || current?.error) return;
    setLoaded(prev => ({ ...prev, [path]: { loading: true, error: null, truncated: false, entries: [] } }));
    api.listLocalDir(path)
      .then(listing => {
        setLoaded(prev => ({
          ...prev,
          [path]: { loading: false, error: null, truncated: listing.truncated, entries: sortEntries(listing.entries) },
        }));
      })
      .catch(err => {
        setLoaded(prev => ({
          ...prev,
          [path]: { loading: false, error: String(err), truncated: false, entries: [] },
        }));
      });
  };

  const toggle = (path: string) => {
    setExpanded(prev => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else {
        next.add(path);
        ensureLoaded(path);
      }
      return next;
    });
  };

  useEffect(() => {
    for (const root of normalizedRoots) ensureLoaded(root);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [normalizedRoots.join("\n")]);

  return (
    <aside className="collab-file-panel">
      <header className="collab-file-head">
        <span>Explorer</span>
        <button
          type="button"
          className="collab-file-add"
          title="Open folder"
          aria-label="Open folder"
          onClick={onOpenFolder}
        >
          <Plus size={14} strokeWidth={2} />
        </button>
      </header>

      {normalizedRoots.length === 0 ? (
        <div className="collab-file-empty">
          <FolderOpen size={20} strokeWidth={1.6} />
          <span>No folder open</span>
        </div>
      ) : (
        <div className="collab-file-tree" role="tree">
          {normalizedRoots.map(root => (
            <TreeNode
              key={root}
              path={root}
              name={basename(root)}
              parent={dirname(root)}
              depth={0}
              isDir
              expanded={expanded}
              loaded={loaded}
              onToggle={toggle}
            />
          ))}
        </div>
      )}
    </aside>
  );
}

function TreeNode({
  path,
  name,
  parent,
  depth,
  isDir,
  expanded,
  loaded,
  onToggle,
}: {
  path: string;
  name: string;
  parent?: string;
  depth: number;
  isDir: boolean;
  expanded: Set<string>;
  loaded: Record<string, LoadedDir>;
  onToggle: (path: string) => void;
}) {
  const open = expanded.has(path);
  const state = loaded[path];
  const entries = state?.entries ?? [];

  return (
    <div className="collab-file-node">
      <div
        className={"collab-file-row" + (isDir ? " folder" : " file")}
        role="treeitem"
        aria-expanded={isDir ? open : undefined}
        style={{ paddingLeft: 8 + depth * 14 }}
        title={path}
      >
        <button
          type="button"
          className="collab-file-chevron"
          tabIndex={-1}
          aria-hidden
          onClick={(e) => {
            e.stopPropagation();
            if (isDir) onToggle(path);
          }}
        >
          {isDir ? (open ? <ChevronDown size={14} /> : <ChevronRight size={14} />) : <span />}
        </button>
        <button
          type="button"
          className="collab-file-label"
          onClick={() => {
            if (isDir) onToggle(path);
          }}
        >
          {isDir
            ? open ? <FolderOpen size={14} strokeWidth={1.7} /> : <Folder size={14} strokeWidth={1.7} />
            : <File size={14} strokeWidth={1.55} />}
          <span className="collab-file-name">{name}</span>
        </button>
      </div>
      {depth === 0 && parent && <div className="collab-file-parent">{parent}</div>}
      {isDir && open && state?.loading && <div className="collab-file-status" style={{ paddingLeft: 28 + depth * 14 }}><Loader2 size={11} strokeWidth={2.25} className="spin" /> Loading</div>}
      {isDir && open && state?.error && <div className="collab-file-status error" style={{ paddingLeft: 28 + depth * 14 }}>{state.error}</div>}
      {isDir && open && state?.truncated && <div className="collab-file-status" style={{ paddingLeft: 28 + depth * 14 }}>Showing first 5,000 entries</div>}
      {isDir && open && entries.map(entry => (
        <TreeNode
          key={entry.path}
          path={entry.path}
          name={entry.name}
          depth={depth + 1}
          isDir={entry.is_dir}
          expanded={expanded}
          loaded={loaded}
          onToggle={onToggle}
        />
      ))}
    </div>
  );
}
