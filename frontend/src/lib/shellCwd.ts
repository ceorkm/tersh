// Live shell working directory per session, reported by the interactive shell
// via OSC 7 (`ESC ] 7 ; file://host/path BEL`). Ephemeral, in-memory only — it
// tracks where the shell actually is right now, so Browse and the upload
// suggestion can anchor to the live directory instead of `~` or a stale,
// auto-detected project root.
//
// Best-effort: OSC 7 is only emitted when the user's shell is configured to do
// so (most modern distro shells are, via PROMPT_COMMAND / precmd). When absent
// the callers fall back to their previous defaults.

const EVENT = "tersh:shell-cwd-updated";
const cwds = new Map<string, string>();

function hasControlChar(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x20 || c === 0x7f) return true;
  }
  return false;
}

/// Parse an OSC 7 payload (`file://host/path`) into a clean absolute path, or
/// null if it isn't one. Hardened against a hostile server smuggling control
/// characters or a non-absolute path through the sequence.
export function parseOsc7(payload: string): string | null {
  if (!payload) return null;
  const m = /^file:\/\/[^/]*(\/.*)$/.exec(payload.trim());
  if (!m || m[1] === undefined) return null;
  let path: string = m[1];
  try {
    path = decodeURIComponent(path);
  } catch {
    /* malformed percent-encoding — keep the raw form */
  }
  if (!path.startsWith("/") || hasControlChar(path)) return null;
  return path.replace(/\/+$/, "") || "/";
}

export function setShellCwd(sessionId: string, payload: string) {
  const path = parseOsc7(payload);
  if (!path) return;
  if (cwds.get(sessionId) === path) return;
  cwds.set(sessionId, path);
  window.dispatchEvent(new CustomEvent(EVENT, { detail: { sessionId, path } }));
}

export function getShellCwd(sessionId: string | null | undefined): string | null {
  if (!sessionId) return null;
  return cwds.get(sessionId) ?? null;
}

export function clearShellCwd(sessionId: string) {
  cwds.delete(sessionId);
}

/// Evict cwd entries for sessions that are no longer live, so the in-memory map
/// doesn't grow unbounded over a long-running process (reconnect mints a fresh
/// session id each time). Mirrors the per-session pruning the Drawer already does.
export function pruneShellCwds(live: Set<string>) {
  for (const id of cwds.keys()) {
    if (!live.has(id)) cwds.delete(id);
  }
}

export function subscribeShellCwd(cb: () => void): () => void {
  window.addEventListener(EVENT, cb);
  return () => window.removeEventListener(EVENT, cb);
}
