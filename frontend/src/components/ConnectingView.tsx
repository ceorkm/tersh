import { useEffect, useRef, useState } from "react";
import { Plug, SquareTerminal, FileText, AlertCircle, ArrowLeft, RefreshCw, Loader2 } from "lucide-react";
import { OsBadge } from "../assets/os-icons";
import type { HostRow } from "../types";

interface ReconnectProps {
  attempt: number;
  maxAttempts: number;
  nextRetryAt: number;
}

interface Props {
  host: HostRow;
  authPrompt?: string;          // password / passphrase request
  error?: string;
  reconnect?: ReconnectProps;
  onConnect?: (secret: string, rememberKeyPassphrase: boolean) => void;
  onCancel?: () => void;
  onRetry?: () => void;
}

export function ConnectingView({ host, authPrompt, error, reconnect, onConnect, onCancel, onRetry }: Props) {
  const [secret, setSecret] = useState("");
  const secretRef = useRef<HTMLInputElement>(null);
  // Default ON: matches the user's "why do I keep getting asked" expectation.
  // First connect auto-saves; subsequent connects skip the prompt entirely.
  const [rememberCredential, setRememberCredential] = useState(true);
  const isAuth = !!authPrompt;
  const isError = !!error;
  const isReconnect = !!reconnect;
  const isLocalTerminal = host.id === "local-terminal";

  // Tick once per second so the "next retry in 4s..." countdown updates.
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!isReconnect) return;
    const id = setInterval(() => setTick(t => (t + 1) % 1000), 1000);
    return () => clearInterval(id);
  }, [isReconnect]);
  const secondsRemaining = reconnect
    ? Math.max(0, Math.ceil((reconnect.nextRetryAt - Date.now()) / 1000))
    : 0;
  const canRemember = host.auth_kind === "key_file" || host.auth_kind === "password";
  const rememberLabel = host.auth_kind === "key_file"
    ? "Save passphrase to this device"
    : "Save password to this device";

  return (
    <div className="connecting-stage">
      <div className="connecting-card">
        <header className="cstage-id">
          {isLocalTerminal ? (
            <span className="cstage-local-badge">
              <SquareTerminal size={32} strokeWidth={1.7} />
            </span>
          ) : (
            <OsBadge os={host.os ?? "linux"} size={56} />
          )}
          <div>
            <div className="id-label">{host.label}</div>
            <div className="id-sub">
              {isLocalTerminal
                ? "local shell"
                : `ssh ${host.username}@${host.hostname}${host.port !== 22 ? `:${host.port}` : ""}`}
            </div>
          </div>
        </header>

        {/* Connection animation */}
        <div className="connection-line">
          <div className={"endpoint source" + (isError ? " err" : "")}>
            <Plug size={16} strokeWidth={2} />
          </div>
          <div className={"line" + (isError ? " err" : "") + (isAuth ? " idle" : "")}>
            <div className="line-fill" />
          </div>
          <div className={"endpoint target" + (isError ? " err" : "")}>
            <SquareTerminal size={16} strokeWidth={2} />
          </div>
        </div>

        {/* State-specific footer */}
        {isReconnect ? (
          <div className="cstage-footer reconnect">
            <div className="cstage-msg">
              <RefreshCw size={14} className="spin" />
              <span>
                {isLocalTerminal ? "Terminal exited. Restarting" : `Connection dropped. Reconnecting (attempt ${reconnect!.attempt}/${reconnect!.maxAttempts})`}
                {secondsRemaining > 0 ? ` in ${secondsRemaining}s…` : "…"}
              </span>
            </div>
            <div className="cstage-actions">
              <button onClick={onCancel}><ArrowLeft size={14} /> Cancel</button>
              {onRetry && (
                <button className="primary" onClick={onRetry}>Retry now</button>
              )}
            </div>
          </div>
        ) : isError ? (
          <div className="cstage-footer error">
            <div className="cstage-msg">
              <AlertCircle size={16} />
              <span>{error}</span>
            </div>
            <div className="cstage-actions">
              <button onClick={onCancel}><ArrowLeft size={14} /> Back</button>
              <button className="primary" onClick={onRetry}>Try again</button>
            </div>
          </div>
        ) : isAuth ? (
          <div className="cstage-footer auth">
            <form
              className="auth-form"
              onSubmit={e => {
                e.preventDefault();
                onConnect?.(secretRef.current?.value || secret, canRemember && rememberCredential);
              }}
            >
              <input
                ref={secretRef}
                type="password"
                placeholder={authPrompt}
                value={secret}
                onChange={e => setSecret(e.target.value)}
                autoFocus
              />
              {canRemember && (
                <label className="remember-row">
                  <input
                    type="checkbox"
                    checked={rememberCredential}
                    onChange={e => setRememberCredential(e.target.checked)}
                  />
                  <span>{rememberLabel}</span>
                </label>
              )}
              <div className="auth-actions">
                <button type="button" onClick={onCancel}>Cancel</button>
                <button type="submit" className="primary">Connect</button>
              </div>
            </form>
            <div className="auth-hint">
              <FileText size={12} />
              <span>Credentials never leave this machine.</span>
            </div>
          </div>
        ) : isLocalTerminal ? (
          <div className="cstage-footer connecting">
            <div className="cstage-msg">
              <Loader2 size={14} strokeWidth={2.25} className="spin" />
              <span>Starting local shell…</span>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
