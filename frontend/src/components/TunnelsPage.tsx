import { useEffect, useRef, useState } from "react";
import { Cable, Plus, Trash2, ArrowRight, Play, Square } from "lucide-react";
import { api } from "../lib/api";
import type { TunnelRow, HostRow } from "../types";
import { ListEmpty } from "./ListEmpty";

interface Props {
  hosts: HostRow[];
  activeSessions?: Map<string, string>; // host_id -> session_id
}

export function TunnelsPage({ hosts, activeSessions }: Props) {
  const [tunnels, setTunnels] = useState<TunnelRow[]>([]);
  const [active, setActive] = useState<Set<string>>(new Set());
  const [creating, setCreating] = useState(false);
  const [label, setLabel] = useState("");
  const [hostId, setHostId] = useState<string>(hosts[0]?.id ?? "");
  const [kind, setKind] = useState<"local" | "remote" | "dynamic">("local");
  const [localPort, setLocalPort] = useState(8080);
  const [remoteHost, setRemoteHost] = useState("localhost");
  const [remotePort, setRemotePort] = useState(80);
  const [err, setErr] = useState<string | null>(null);
  const mounted = useRef(true);
  const loading = useRef(false);

  const load = async () => {
    if (loading.current) return;
    loading.current = true;
    try {
      const next = await api.listTunnels();
      if (!mounted.current) return;
      setTunnels(next);
      setErr(null);
    } catch (e) {
      if (mounted.current) setErr(`Could not load tunnels: ${e}`);
    }
    try {
      const next = await api.activeTunnels();
      if (!mounted.current) return;
      setActive(new Set(next));
    } catch (e) {
      if (mounted.current) setErr(`Could not load active tunnels: ${e}`);
    } finally {
      loading.current = false;
    }
  };
  useEffect(() => {
    mounted.current = true;
    load();
    const id = setInterval(load, 3000);
    return () => {
      mounted.current = false;
      clearInterval(id);
    };
  }, []);
  useEffect(() => {
    if (!hostId && hosts[0]) setHostId(hosts[0].id);
  }, [hostId, hosts]);

  const save = async () => {
    if (!label.trim() || !hostId) return;
    if (localPort < 1 || localPort > 65535) {
      setErr("Local port must be between 1 and 65535.");
      return;
    }
    if (kind !== "dynamic" && (remotePort < 1 || remotePort > 65535 || !remoteHost.trim())) {
      setErr("Remote host and port are required.");
      return;
    }
    try {
      await api.addTunnel({
        label: label.trim(), host_id: hostId, kind,
        local_port: localPort,
        remote_host: kind === "dynamic" ? null : remoteHost.trim(),
        remote_port: kind === "dynamic" ? null : remotePort,
      });
      if (!mounted.current) return;
      setLabel(""); setCreating(false); setErr(null);
      await load();
    } catch (e) {
      if (mounted.current) setErr(String(e));
    }
  };

  const hostLabel = (id: string) => hosts.find(h => h.id === id)?.label ?? id;

  return (
    <>
      <div className="main-toolbar">
        <button className="toolbar-btn primary-ghost" onClick={() => setCreating(true)}>
          <Plus size={14} strokeWidth={2} /> New tunnel
        </button>
        <div className="grow" />
        <span className="count-badge">{tunnels.length} tunnel{tunnels.length === 1 ? "" : "s"}</span>
      </div>

      {creating && (
        <div className="inline-form vertical">
          <input placeholder="Tunnel name" value={label} onChange={e => setLabel(e.target.value)} autoFocus />
          <div className="form-row">
            <select value={kind} onChange={e => setKind(e.target.value as typeof kind)} style={{ width: 130 }}>
              <option value="local">Local (→)</option>
              <option value="remote">Remote (←)</option>
              <option value="dynamic">Dynamic (SOCKS)</option>
            </select>
            <select value={hostId} onChange={e => setHostId(e.target.value)}>
              <option value="">Choose host…</option>
              {hosts.map(h => <option key={h.id} value={h.id}>{h.label}</option>)}
            </select>
          </div>
          <div className="form-row">
            <label className="micro">
              <span>Local port</span>
              <input type="number" value={localPort} onChange={e => setLocalPort(+e.target.value || 0)} min={1} max={65535} />
            </label>
            {kind !== "dynamic" && (
              <>
                <label className="micro grow">
                  <span>Remote host</span>
                  <input value={remoteHost} onChange={e => setRemoteHost(e.target.value)} />
                </label>
                <label className="micro">
                  <span>Remote port</span>
                  <input type="number" value={remotePort} onChange={e => setRemotePort(+e.target.value || 0)} min={1} max={65535} />
                </label>
              </>
            )}
          </div>
          {err && <div className="err inline">{err}</div>}
          <div className="inline-form-actions">
            <button onClick={() => { setCreating(false); setErr(null); }}>Cancel</button>
            <button className="primary" onClick={save} disabled={!label.trim() || !hostId}>Save tunnel</button>
          </div>
        </div>
      )}

      {err && !creating && (
        <div className="err inline">{err}</div>
      )}

      <div className="content-scroll">
        {tunnels.length === 0 && !creating ? (
          <ListEmpty
            icon={<Cable size={44} strokeWidth={1.5} />}
            title="No port forwards"
            hint="Forward a local port to a remote service through SSH."
            cta="New tunnel"
            onCta={() => setCreating(true)}
          />
        ) : (
          <div className="card-list">
            {tunnels.map(t => (
              <article className="kc-card" key={t.id}>
                <div className="kc-icon"><Cable size={20} strokeWidth={1.75} /></div>
                <div className="kc-meta">
                  <div className="kc-head">
                    <h3>{t.label}</h3>
                    <span className="kind-pill">{t.kind}</span>
                  </div>
                  <div className="tunnel-route">
                    <code>:{t.local_port}</code>
                    <ArrowRight size={12} />
                    <code>{hostLabel(t.host_id)}</code>
                    {t.remote_host && (
                      <>
                        <ArrowRight size={12} />
                        <code>{t.remote_host}:{t.remote_port}</code>
                      </>
                    )}
                  </div>
                </div>
                <div className="kc-actions">
                  {active.has(t.id) ? (
                    <button className="ghost" title="Stop" onClick={async () => {
                      try {
                        await api.stopTunnel(t.id);
                        if (!mounted.current) return;
                        setErr(null);
                        await load();
                      }
                      catch (e) { if (mounted.current) setErr(String(e)); }
                    }}><Square size={14} /></button>
                  ) : (
                    <button className="ghost" title="Start (host must be connected)" onClick={async () => {
                      const sid = activeSessions?.get(t.host_id);
                      if (!sid) { setErr(`Connect to "${hostLabel(t.host_id)}" first to start this tunnel.`); return; }
                      try {
                        await api.startTunnel(t.id, sid);
                        if (!mounted.current) return;
                        setErr(null);
                        await load();
                      }
                      catch (e) { if (mounted.current) setErr(String(e)); }
                    }}><Play size={14} /></button>
                  )}
                  <button className="ghost danger" onClick={async () => {
                    if (confirm(`Delete tunnel "${t.label}"?`)) {
                      try {
                        await api.deleteTunnel(t.id);
                        if (!mounted.current) return;
                        setErr(null);
                        await load();
                      } catch (e) {
                        if (mounted.current) setErr(String(e));
                      }
                    }
                  }}><Trash2 size={14} /></button>
                </div>
              </article>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
