import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { CollaboratorTerminalGuest } from "./components/CollaboratorTerminalGuest";
import { applyAppearance, clearGlobalAppearance, DEFAULT_FONT, DEFAULT_FONT_SIZE } from "./lib/appearance";
import "./styles.css";

// ── DEBUG: surface boot-time errors without nuking a running SSH session.
// Runtime errors are logged; replacing the whole root after startup makes a
// recoverable async failure look like the terminal itself crashed.
let booted = false;
const escapeHtml = (s: string) =>
  s.replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] ?? c));
const showFatal = (where: string, msg: string, stack?: string) => {
  if (booted) {
    console.error(`tersh runtime error (${where})`, msg, stack);
    return;
  }
  const root = document.getElementById("root") || document.body;
  root.innerHTML = `
    <div style="padding:32px;font-family:ui-monospace,monospace;color:#d33;background:#111;min-height:100vh;color:#f88;">
      <div style="font-size:13px;opacity:.7;margin-bottom:8px;">tersh failed to start (${where})</div>
      <pre style="font-size:13px;white-space:pre-wrap;">${escapeHtml(msg)}\n${escapeHtml(stack || "")}</pre>
    </div>`;
};
window.addEventListener("error", (e) => {
  showFatal("window.error", e.message, e.error?.stack);
});
window.addEventListener("unhandledrejection", (e) => {
  const r: any = e.reason;
  showFatal("unhandled promise", r?.message || String(r), r?.stack);
});

try {
  // App chrome must never boot from persisted terminal themes. A per-host
  // yellow/light terminal theme can be saved in localStorage; applying that
  // before React mounts makes the whole app flash/stick yellow. App.tsx keeps
  // host terminal themes scoped to TerminalView, so boot dark here.
  clearGlobalAppearance();
  applyAppearance({ themeId: "tersh-dark", fontId: DEFAULT_FONT, fontSize: DEFAULT_FONT_SIZE });

  // NOTE: <React.StrictMode> intentionally removed. It double-invokes effects
  // in dev which double-registered the Tauri drag-drop handler → every drop
  // fired twice (filename pasted twice). Re-enable only after every effect
  // is verified safe to re-run with a cancel token.
  const params = new URLSearchParams(window.location.search);
  const root =
    params.get("view") === "collab-terminal"
      ? <CollaboratorTerminalGuest />
      : <App />;
  ReactDOM.createRoot(document.getElementById("root")!).render(root);
  booted = true;
  // suppress unused import warning
  void React;
} catch (e: any) {
  showFatal("boot", e?.message || String(e), e?.stack);
}
