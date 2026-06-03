import { useEffect, useRef, useState } from "react";
import { Code2, Plus, Trash2, Play, Edit3 } from "lucide-react";
import { api } from "../lib/api";
import type { SnippetRow } from "../types";
import { ListEmpty } from "./ListEmpty";

interface Props {
  activeSessionId?: string | null;
}

export function SnippetsPage({ activeSessionId }: Props) {
  const [snippets, setSnippets] = useState<SnippetRow[]>([]);
  const [editingId, setEditingId] = useState<string | "new" | null>(null);
  const [label, setLabel] = useState("");
  const [command, setCommand] = useState("");
  const [description, setDescription] = useState("");
  const [tags, setTags] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const mounted = useRef(true);

  const load = async () => {
    try {
      const next = await api.listSnippets();
      if (!mounted.current) return;
      setSnippets(next);
      setErr(null);
    } catch (e) {
      if (mounted.current) setErr(`Could not load snippets: ${e}`);
    }
  };
  useEffect(() => {
    mounted.current = true;
    load();
    return () => {
      mounted.current = false;
    };
  }, []);

  const resetForm = () => { setLabel(""); setCommand(""); setDescription(""); setTags(""); };

  const startNew = () => {
    resetForm(); setEditingId("new");
  };
  const startEdit = (s: SnippetRow) => {
    setLabel(s.label); setCommand(s.command);
    setDescription(s.description ?? ""); setTags(s.tags ?? "");
    setEditingId(s.id);
  };
  const save = async () => {
    if (!label.trim() || !command.trim()) return;
    const input = {
      label: label.trim(), command: command.trim(),
      description: description.trim() || null,
      tags: tags.trim() || null,
    };
    try {
      if (editingId === "new") await api.addSnippet(input);
      else if (editingId) await api.updateSnippet(editingId, input);
      if (!mounted.current) return;
      setEditingId(null); resetForm(); setErr(null); await load();
    } catch (e) {
      if (mounted.current) setErr(String(e));
    }
  };
  const run = async (s: SnippetRow) => {
    if (!activeSessionId) {
      setErr("Connect to a host first to run a snippet.");
      return;
    }
    try {
      setErr(null);
      await api.runSnippet(activeSessionId, s.id);
    }
    catch (e) {
      if (mounted.current) setErr(`Run failed: ${e}`);
    }
  };

  return (
    <>
      <div className="main-toolbar">
        <button className="toolbar-btn primary-ghost" onClick={startNew}>
          <Plus size={14} strokeWidth={2} /> New snippet
        </button>
        <div className="grow" />
        <span className="count-badge">{snippets.length} snippet{snippets.length === 1 ? "" : "s"}</span>
      </div>

      {editingId !== null && (
        <div className="inline-form vertical">
          <input
            placeholder="Snippet name (e.g. update apt)"
            value={label}
            onChange={e => setLabel(e.target.value)}
            autoFocus
          />
          <textarea
            placeholder="Command — multiline ok"
            value={command}
            onChange={e => setCommand(e.target.value)}
            rows={3}
          />
          <input
            placeholder="Description (optional)"
            value={description}
            onChange={e => setDescription(e.target.value)}
          />
          <input
            placeholder="Tags, comma-separated"
            value={tags}
            onChange={e => setTags(e.target.value)}
          />
          <div className="inline-form-actions">
            <button onClick={() => { setEditingId(null); resetForm(); }}>Cancel</button>
            <button className="primary" onClick={save} disabled={!label.trim() || !command.trim()}>
              {editingId === "new" ? "Add snippet" : "Save"}
            </button>
          </div>
        </div>
      )}
      {err && <div className="err inline">{err}</div>}

      <div className="content-scroll">
        {snippets.length === 0 && editingId === null ? (
          <ListEmpty
            icon={<Code2 size={44} strokeWidth={1.5} />}
            title="No snippets yet"
            hint="Save commands you use often. Run them on any connected host in one click."
            cta="New snippet"
            onCta={startNew}
          />
        ) : (
          <div className="card-list">
            {snippets.map(s => (
              <article className="kc-card" key={s.id}>
                <div className="kc-icon"><Code2 size={20} strokeWidth={1.75} /></div>
                <div className="kc-meta">
                  <div className="kc-head">
                    <h3>{s.label}</h3>
                    {s.tags && (
                      <span className="tags-row">
                        {s.tags.split(",").map(t => t.trim()).filter(Boolean).map(t => (
                          <span key={t} className="tag">{t}</span>
                        ))}
                      </span>
                    )}
                  </div>
                  <pre className="snippet-cmd">{s.command}</pre>
                  {s.description && <div className="snippet-desc">{s.description}</div>}
                </div>
                <div className="kc-actions">
                  <button className="primary" onClick={() => run(s)} disabled={!activeSessionId} title={activeSessionId ? "Run on active session" : "Connect to a host to run"}>
                    <Play size={13} fill="currentColor" /> Run
                  </button>
                  <button className="ghost" onClick={() => startEdit(s)} title="Edit"><Edit3 size={14} /></button>
                  <button className="ghost danger" onClick={async () => {
                    if (confirm(`Delete snippet "${s.label}"?`)) {
                      try {
                        await api.deleteSnippet(s.id);
                        if (!mounted.current) return;
                        setErr(null);
                        await load();
                      } catch (e) {
                        if (mounted.current) setErr(String(e));
                      }
                    }
                  }} title="Delete"><Trash2 size={14} /></button>
                </div>
              </article>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
