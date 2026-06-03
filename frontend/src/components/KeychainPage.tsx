import { useEffect, useRef, useState } from "react";
import { KeyRound, Plus, Trash2, Copy, Check, Download, Upload, Hash, LockKeyhole } from "lucide-react";
import { api } from "../lib/api";
import type { KeyRow } from "../types";
import { ListEmpty } from "./ListEmpty";

export function KeychainPage() {
  const [keys, setKeys] = useState<KeyRow[]>([]);
  const [creating, setCreating] = useState(false);
  const [newLabel, setNewLabel] = useState("");
  const [newComment, setNewComment] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [remembered, setRemembered] = useState<Set<string>>(new Set());
  const copiedTimer = useRef<number | null>(null);
  const mounted = useRef(true);

  const load = async () => {
    try {
      const [keyRows, activeKeyIds] = await Promise.all([
        api.listKeys(),
        api.listActiveKeypassKeys(),
      ]);
      if (!mounted.current) return;
      setKeys(keyRows);
      setRemembered(new Set(activeKeyIds));
    }
    catch (e) {
      if (mounted.current) setErr(String(e));
    }
  };
  useEffect(() => {
    mounted.current = true;
    load();
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    return () => {
      if (copiedTimer.current !== null) window.clearTimeout(copiedTimer.current);
    };
  }, []);

  const generate = async () => {
    if (!newLabel.trim()) return;
    setBusy(true); setErr(null);
    try {
      await api.generateKey(newLabel.trim(), newComment.trim() || undefined);
      if (!mounted.current) return;
      setNewLabel(""); setNewComment(""); setCreating(false);
      await load();
    } catch (e) {
      if (mounted.current) setErr(String(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  const importKey = async () => {
    try {
      const path = await api.pickFile();
      if (!mounted.current) return;
      if (!path) return;
      const label = path.split("/").pop() ?? "imported";
      await api.importKey(label, path);
      if (!mounted.current) return;
      await load();
    } catch (e) {
      if (mounted.current) setErr(String(e));
    }
  };

  const copyPub = async (k: KeyRow) => {
    try {
      await navigator.clipboard.writeText(k.public_key);
      if (!mounted.current) return;
      setErr(null);
      setCopiedId(k.id);
      if (copiedTimer.current !== null) window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => {
        copiedTimer.current = null;
        setCopiedId(null);
      }, 1400);
    } catch (e) {
      if (mounted.current) setErr(`Copy failed: ${e}`);
    }
  };

  return (
    <>
      <div className="main-toolbar">
        <button className="toolbar-btn primary-ghost" onClick={() => setCreating(true)}>
          <Plus size={14} strokeWidth={2} /> Generate key
        </button>
        <button className="toolbar-btn" onClick={importKey}>
          <Upload size={14} strokeWidth={1.75} /> Import key
        </button>
        <div className="grow" />
        <span className="count-badge">{keys.length} key{keys.length === 1 ? "" : "s"}</span>
      </div>

      {creating && (
        <div className="inline-form vertical">
          <input
            placeholder="Key label (e.g. work-laptop)"
            value={newLabel}
            onChange={e => setNewLabel(e.target.value)}
            autoFocus
            onKeyDown={e => { if (e.key === "Enter") generate(); if (e.key === "Escape") setCreating(false); }}
          />
          <input
            placeholder="Comment (optional, e.g. you@laptop)"
            value={newComment}
            onChange={e => setNewComment(e.target.value)}
          />
          <div className="inline-form-actions">
            <button onClick={() => setCreating(false)}>Cancel</button>
            <button className="primary" onClick={generate} disabled={busy || !newLabel.trim()}>
              {busy ? "Generating…" : "Generate ed25519"}
            </button>
          </div>
        </div>
      )}

      {err && <div className="err inline">{err}</div>}

      <div className="content-scroll">
        {keys.length === 0 ? (
          <ListEmpty
            icon={<KeyRound size={44} strokeWidth={1.5} />}
            title="No keys yet"
            hint="Generate a new ed25519 key, or import an existing one."
            cta="Generate key"
            onCta={() => setCreating(true)}
          />
        ) : (
          <div className="card-list">
            {keys.map(k => (
              <article className="kc-card" key={k.id}>
                <div className="kc-icon">
                  <KeyRound size={20} strokeWidth={1.75} />
                </div>
                <div className="kc-meta">
                  <div className="kc-head">
                    <h3>{k.label}</h3>
                    <span className="kind-pill">{k.kind}</span>
                  </div>
                  <div className="kc-fp">
                    <Hash size={11} />
                    <span>{k.fingerprint}</span>
                  </div>
                  {k.private_path && (
                    <div className="kc-path"><Download size={11} /> {k.private_path}</div>
                  )}
                </div>
                <div className="kc-actions">
                  <button className="ghost" onClick={() => copyPub(k)} title="Copy public key">
                    {copiedId === k.id ? <Check size={14} /> : <Copy size={14} />}
                    {copiedId === k.id ? "Copied" : "Copy"}
                  </button>
                  {remembered.has(k.id) && (
                    <button
	                      className="ghost"
		                      onClick={async () => {
		                        try {
		                          await api.clearKeyPassphrase(k.id);
		                          if (!mounted.current) return;
		                          setErr(null);
		                          await load();
		                        } catch (e) {
		                          if (mounted.current) setErr(String(e));
		                        }
		                      }}
                      title="Clear remembered passphrase"
                    >
                      <LockKeyhole size={14} /> Forget passphrase
                    </button>
                  )}
                  <button
                    className="ghost danger"
	                    onClick={async () => {
		                      if (confirm(`Delete key "${k.label}"? (Private file on disk is NOT deleted.)`)) {
		                        try {
		                          await api.deleteKey(k.id);
		                          if (!mounted.current) return;
		                          setErr(null);
		                          await load();
		                        } catch (e) {
		                          if (mounted.current) setErr(String(e));
		                        }
		                      }
	                    }}
                    title="Remove from vault"
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </article>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
