import { useEffect, useRef, useState } from "react";
import { ShieldCheck, Hash, Clock, Copy, Check, Search } from "lucide-react";
import { api } from "../lib/api";
import type { KnownHostRow, HostRow } from "../types";
import { ListEmpty } from "./ListEmpty";

interface Props {
  hosts: HostRow[];
}

function relTime(seconds: number): string {
  const d = new Date(seconds * 1000);
  const diff = (Date.now() - d.getTime()) / 1000;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 86400 * 30) return `${Math.floor(diff / 86400)}d ago`;
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

// "SHA256:abc…" → ["SHA256", "abc…"]. The algo prefix is de-emphasized so the
// hash (the part that actually matters when comparing) reads as the content.
function splitFingerprint(fp: string): [string, string] {
  const i = fp.indexOf(":");
  return i > 0 ? [fp.slice(0, i), fp.slice(i + 1)] : ["", fp];
}

export function KnownHostsPage({ hosts }: Props) {
  const [rows, setRows] = useState<KnownHostRow[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [copied, setCopied] = useState<string | null>(null);
  const mounted = useRef(true);
  const copyTimer = useRef<number | null>(null);

  const load = async () => {
    try {
      const next = await api.listKnownHosts();
      if (!mounted.current) return;
      setRows(next);
      setErr(null);
    } catch (e) {
      if (mounted.current) setErr(`Could not load known hosts: ${e}`);
    }
  };
  useEffect(() => {
    mounted.current = true;
    load();
    return () => {
      mounted.current = false;
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    };
  }, []);

  const hostLabel = (id: string) => hosts.find(h => h.id === id)?.label ?? `<unknown ${id.slice(0, 6)}>`;
  const hostAddr  = (id: string) => {
    const h = hosts.find(h => h.id === id);
    return h ? `${h.username}@${h.hostname}` : null;
  };

  const copyFingerprint = (fp: string) => {
    void navigator.clipboard?.writeText(fp).catch(() => {});
    setCopied(fp);
    if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(() => {
      if (mounted.current) setCopied(null);
    }, 1200);
  };

  const q = query.trim().toLowerCase();
  const visible = q
    ? rows.filter(r =>
        hostLabel(r.host_id).toLowerCase().includes(q) ||
        (hostAddr(r.host_id) ?? "").toLowerCase().includes(q) ||
        r.fingerprint.toLowerCase().includes(q),
      )
    : rows;

  return (
    <>
      <div className="main-toolbar">
        <span className="toolbar-hint">First-use fingerprints captured by Tersh. Changed host keys are blocked until a native verification flow is available.</span>
        <div className="grow" />
        {rows.length > 0 && (
          <div className="kh-search">
            <Search size={13} strokeWidth={2} />
            <input
              type="text"
              placeholder="Filter hosts"
              value={query}
              onChange={e => setQuery(e.target.value)}
              spellCheck={false}
            />
          </div>
        )}
        <span className="count-badge">{rows.length} fingerprint{rows.length === 1 ? "" : "s"}</span>
      </div>
      {err && <div className="err inline">{err}</div>}

      <div className="content-scroll">
        {rows.length === 0 ? (
          <ListEmpty
            icon={<ShieldCheck size={44} strokeWidth={1.5} />}
            title="No known hosts yet"
            hint="Server fingerprints are recorded on first connect. They appear here."
          />
        ) : visible.length === 0 ? (
          <ListEmpty
            icon={<Search size={44} strokeWidth={1.5} />}
            title="No matches"
            hint={`Nothing matches “${query.trim()}”.`}
          />
        ) : (
          <div className="card-list">
            {visible.map(r => {
              const [algo, hash] = splitFingerprint(r.fingerprint);
              const isCopied = copied === r.fingerprint;
              return (
                <article className="kc-card" key={`${r.host_id}-${r.fingerprint}`}>
                  <div className="kc-icon"><ShieldCheck size={20} strokeWidth={1.75} /></div>
                  <div className="kc-meta">
                    <div className="kc-head">
                      <h3>{hostLabel(r.host_id)}</h3>
                      <span className="kind-pill trusted">Trusted</span>
                      {hostAddr(r.host_id) && (
                        <span className="kc-sub-inline">{hostAddr(r.host_id)}</span>
                      )}
                    </div>
                    <div className="kc-fp" title={r.fingerprint}>
                      <Hash size={11} />
                      {algo && <span className="kc-fp-algo">{algo}</span>}
                      <span className="kc-fp-hash">{hash}</span>
                    </div>
                    <div className="kc-path" title={new Date(r.first_seen * 1000).toLocaleString()}>
                      <Clock size={11} /> First seen {relTime(r.first_seen)}
                    </div>
                  </div>
                  <div className="kc-actions">
                    <button
                      type="button"
                      className={`ghost icon-only${isCopied ? " copied" : ""}`}
                      aria-label="Copy fingerprint"
                      title={isCopied ? "Copied" : "Copy fingerprint"}
                      onClick={() => copyFingerprint(r.fingerprint)}
                    >
                      {isCopied ? <Check size={14} strokeWidth={2.25} /> : <Copy size={14} strokeWidth={2} />}
                    </button>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </div>
    </>
  );
}
