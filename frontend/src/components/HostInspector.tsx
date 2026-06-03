import { useEffect, useRef, useState } from "react";
import {
  ArrowRight, FolderClosed, User, KeyRound, Plus, Code2,
  Network, Variable, ChevronDown, LockKeyhole,
} from "lucide-react";
import { api } from "../lib/api";
import { OsBadge, OS_LIST, OS_LABELS } from "../assets/os-icons";
import type { AddHostInput, HostRow, OsKind } from "../types";

interface Props {
  hosts: HostRow[];
  initialHost?: HostRow | null;
  onClose: () => void;
  onSaved: (host?: HostRow, password?: string) => void;
  onSavedFallback?: (h: HostRow) => void;
}

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export function HostInspector({ hosts, initialHost, onClose, onSaved, onSavedFallback }: Props) {
  const [label, setLabel] = useState(initialHost?.label ?? "");
  const [hostname, setHostname] = useState(initialHost?.hostname ?? "");
  const [port, setPort] = useState(initialHost?.port ?? 22);
  const [username, setUsername] = useState(initialHost?.username ?? "root");
  const [password, setPassword] = useState("");
  const passwordRef = useRef<HTMLInputElement>(null);
  const [authKind, setAuthKind] = useState<"password" | "key_file">(initialHost?.auth_kind ?? "password");
  const [keyPath, setKeyPath] = useState(initialHost?.key_path ?? "");
  const [groupName, setGroupName] = useState(initialHost?.group_name ?? "");
  const [jumpHostId, setJumpHostId] = useState(initialHost?.jump_host_id ?? "");
  const [envJson, setEnvJson] = useState(initialHost?.env_json ?? "");
  const [startupSnippet, setStartupSnippet] = useState(initialHost?.startup_snippet ?? "");
  const [os, setOs] = useState<OsKind>(initialHost?.os ?? "linux");
  const [osPickerOpen, setOsPickerOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [hasSavedPassword, setHasSavedPassword] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    if (!initialHost || initialHost.auth_kind !== "password") {
      setPassword("");
      setHasSavedPassword(false);
      return;
    }

    let cancelled = false;
    setPassword("");
    api.hasHostPassword(initialHost.id)
      .then((exists) => {
        if (!cancelled && mounted.current) {
          setHasSavedPassword(exists);
        }
      })
      .catch(() => {
        if (!cancelled && mounted.current) {
          setHasSavedPassword(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [initialHost]);

  const canConnect = hostname.trim() && username.trim() && (authKind === "password" || keyPath.trim());

  const submit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!canConnect) return;
    const env = envJson.trim();
    if (env) {
      try {
        const parsed = JSON.parse(env);
        if (!parsed || Array.isArray(parsed) || typeof parsed !== "object") {
          setErr("Environment must be a JSON object.");
          return;
        }
        if (!Object.values(parsed).every(v => typeof v === "string")) {
          setErr("Environment values must be strings.");
          return;
        }
      } catch {
        setErr("Environment must be valid JSON.");
        return;
      }
    }
    setBusy(true); setErr(null);
    const input: AddHostInput = {
      label: label.trim(),
      hostname: hostname.trim(),
      port,
      username: username.trim(),
      auth_kind: authKind,
      key_path: authKind === "key_file" ? keyPath.trim() : null,
      group_name: groupName.trim() || null,
      os,
      jump_host_id: jumpHostId || null,
      env_json: env || null,
      startup_snippet: startupSnippet.trim() || null,
    };
    try {
      const passwordValue = passwordRef.current?.value || password;
      const hostId = initialHost ? initialHost.id : await api.addHost(input);
      if (initialHost) await api.updateHost(initialHost.id, input);
      if (initialHost && authKind === "password" && passwordValue) {
        try {
          await api.setHostPassword(hostId, passwordValue);
        } catch (saveErr) {
          if (mounted.current) setErr(`Host saved, but the password was not saved to this device: ${String(saveErr)}`);
          return;
        }
      }
      if (!mounted.current) return;
      onSaved({ ...input, id: hostId }, passwordValue || undefined);
    } catch (saveErr) {
      if (onSavedFallback && !isTauriRuntime()) {
        const fallback: HostRow = { ...input, id: initialHost?.id ?? crypto.randomUUID() };
        if (!mounted.current) return;
        onSavedFallback(fallback);
      } else {
        if (mounted.current) setErr(String(saveErr));
      }
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  return (
    <>
      <div className="inspector-scrim" onClick={onClose} />
      <aside className="inspector" role="dialog" aria-label={initialHost ? "Edit Host" : "New Host"}>
        <header className="inspector-header">
          <div className="ih-title">
            <h1>{initialHost ? "Edit Host" : "New Host"}</h1>
            <div className="ih-vault">
              <span>Personal vault</span>
              <ChevronDown size={13} strokeWidth={2} />
            </div>
          </div>
          <div className="ih-actions">
            <button className="ih-iconbtn" type="button" onClick={onClose} title="Close">
              <ArrowRight size={16} strokeWidth={2} />
            </button>
          </div>
        </header>

        <form className="inspector-body" onSubmit={submit}>
          {/* ─── Address ─── */}
          <Card title="Address">
            <div className="address-row">
              <button
                type="button"
                className="os-picker-btn"
                onClick={() => setOsPickerOpen(o => !o)}
                aria-label="Choose OS"
                title={`OS: ${initialHost?.os ? OS_LABELS[os] : "Auto-detect after first connection"} — click to change`}
              >
                <OsBadge os={os} size={40} />
              </button>
              <input
                className="address-input"
                placeholder="IP or Hostname"
                value={hostname}
                onChange={e => setHostname(e.target.value)}
                spellCheck={false}
              />
            </div>
            {osPickerOpen && (
              <div className="os-picker-grid">
                {OS_LIST.map(o => (
                  <button
                    type="button"
                    key={o}
                    className={"os-picker-tile " + (o === os ? "active" : "")}
                    onClick={() => { setOs(o); setOsPickerOpen(false); }}
                    title={OS_LABELS[o]}
                  >
                    <OsBadge os={o} size={28} />
                    <span>{OS_LABELS[o]}</span>
                  </button>
                ))}
              </div>
            )}
          </Card>

          {/* ─── General ─── */}
          <Card title="General">
            <Row>
              <input
                placeholder="Label (optional)"
                value={label}
                onChange={e => setLabel(e.target.value)}
                autoFocus
              />
            </Row>
            <Row icon={<FolderClosed size={14} />}>
              <input
                placeholder="Parent Group"
                value={groupName}
                onChange={e => setGroupName(e.target.value)}
              />
            </Row>
          </Card>

          {/* ─── SSH on port ─── */}
          <Card>
            <div className="ssh-on-row">
              <span className="row-text">SSH on</span>
              <input
                className="port-input"
                type="number"
                value={port}
                onChange={e => setPort(parseInt(e.target.value, 10) || 22)}
                min={1}
                max={65535}
              />
              <span className="row-text">port</span>
            </div>
            <hr className="card-divider" />
            <div className="card-subtitle">Credentials</div>
            <Row icon={<User size={14} />}>
              <input
                placeholder="Username"
                value={username}
                onChange={e => setUsername(e.target.value)}
              />
            </Row>
            {authKind === "password" && (
              <Row icon={<LockKeyhole size={14} />}>
                <input
                  ref={passwordRef}
                  type="text"
                  placeholder={initialHost && hasSavedPassword ? "Saved password (leave blank to keep)" : "Password"}
                  value={password}
                  onChange={e => setPassword(e.target.value)}
                />
              </Row>
            )}
            <button
              type="button"
              className="row-action subtle"
              onClick={() => setAuthKind(a => a === "password" ? "key_file" : "password")}
            >
              <Plus size={14} strokeWidth={2} />
              <span>{authKind === "password" ? "Use key file" : "Use password prompt"}</span>
            </button>
            {authKind === "key_file" && (
              <Row icon={<KeyRound size={14} />}>
                <input
                  placeholder="/Users/you/.ssh/id_ed25519"
                  value={keyPath}
                  onChange={e => setKeyPath(e.target.value)}
                />
              </Row>
            )}
          </Card>

          <Card title="Advanced">
            <Row icon={<Network size={14} />}>
              <select value={jumpHostId} onChange={e => setJumpHostId(e.target.value)}>
                <option value="">No jump host</option>
                {hosts
                  .filter(h => h.id !== initialHost?.id)
                  .map(h => <option key={h.id} value={h.id}>{h.label}</option>)}
              </select>
            </Row>
            <Row icon={<Variable size={14} />}>
              <textarea
                placeholder={'Environment JSON, e.g. {"TERM":"xterm-256color"}'}
                value={envJson}
                onChange={e => setEnvJson(e.target.value)}
                rows={3}
              />
            </Row>
            <Row icon={<Code2 size={14} />}>
              <textarea
                placeholder="Startup snippet"
                value={startupSnippet}
                onChange={e => setStartupSnippet(e.target.value)}
                rows={3}
              />
            </Row>
          </Card>

          {err && <div className="ih-err">{err}</div>}

          <button
            type="submit"
            className="ih-connect"
            disabled={!canConnect || busy}
          >
            {busy ? "Saving…" : initialHost ? "Save" : "Save and connect"}
          </button>
        </form>
      </aside>
    </>
  );
}

function Card({
  title,
  children,
  flat,
}: { title?: string; children: React.ReactNode; flat?: boolean }) {
  return (
    <section className={"ih-card" + (flat ? " flat" : "")}>
      {title && <h3 className="ih-card-title">{title}</h3>}
      <div className="ih-card-body">{children}</div>
    </section>
  );
}

function Row({
  icon,
  children,
  trailing,
}: { icon?: React.ReactNode; children: React.ReactNode; trailing?: React.ReactNode }) {
  return (
    <div className="ih-row">
      {icon && <span className="ih-row-icon">{icon}</span>}
      <div className="ih-row-content">{children}</div>
      {trailing && <span className="ih-row-trailing">{trailing}</span>}
    </div>
  );
}
