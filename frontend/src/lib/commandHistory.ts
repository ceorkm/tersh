import type { HostRow } from "../types";

const KEY = "tersh:command-history";
const EVENT = "tersh:command-history-updated";
const MAX_ITEMS = 200;

export interface CommandHistoryItem {
  command: string;
  hostId: string;
  hostLabel: string;
  at: number;
}

const CONTROL_CHARS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/;
const SECRET_WORD = /(?:password|passwd|passphrase|token|secret|credential|api[-_]?key|access[-_]?key|private[-_]?key|auth[-_]?key|session[-_]?key|bearer|otp|mfa|2fa)/i;
const SECRET_ASSIGNMENT = /(?:^|[\s;])(?:export\s+)?[A-Z0-9_]*(?:PASSWORD|PASSWD|PASSPHRASE|TOKEN|SECRET|CREDENTIAL|API_KEY|ACCESS_KEY|PRIVATE_KEY|AUTH_KEY|SESSION_KEY)[A-Z0-9_]*\s*=/i;
const SECRET_FLAG = /(?:^|\s)--?(?:p|password|pass|passphrase|token|secret|api-key|access-key|private-key)(?:=|\s+\S)/i;
const SECRET_COMMAND = /(?:^|[\s;|&])(?:sshpass|pass|op|gopass|security|read\s+-[^\r\n;|&]*s)\b/i;
const INLINE_AUTHORITY_SECRET = /:\/\/[^/\s:@]+:[^/\s@]+@/;

function isSafeForHistory(command: string): boolean {
  if (CONTROL_CHARS.test(command)) return false;
  if (SECRET_ASSIGNMENT.test(command)) return false;
  if (SECRET_FLAG.test(command)) return false;
  if (SECRET_COMMAND.test(command)) return false;
  if (INLINE_AUTHORITY_SECRET.test(command)) return false;
  if (/^(?:sudo|su|doas|login|passwd)\b/i.test(command)) return false;
  if (/^(?:export|set|setenv)\b/i.test(command) && SECRET_WORD.test(command)) return false;
  return true;
}

function read(): CommandHistoryItem[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as CommandHistoryItem[];
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(item =>
      item &&
      typeof item.command === "string" &&
      typeof item.hostId === "string" &&
      typeof item.hostLabel === "string" &&
      Number.isFinite(item.at),
    );
  } catch {
    return [];
  }
}

function write(items: CommandHistoryItem[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(items.slice(0, MAX_ITEMS)));
    window.dispatchEvent(new CustomEvent(EVENT));
  } catch {}
}

export function listCommandHistory(): CommandHistoryItem[] {
  return read();
}

export function subscribeCommandHistory(cb: () => void): () => void {
  window.addEventListener(EVENT, cb);
  window.addEventListener("storage", cb);
  return () => {
    window.removeEventListener(EVENT, cb);
    window.removeEventListener("storage", cb);
  };
}

export function recordCommand(command: string, host: HostRow) {
  const normalized = command.trim();
  if (!normalized) return;
  if (normalized.length > 2000) return;
  if (!isSafeForHistory(normalized)) return;
  const items = read().filter(item => item.command !== normalized);
  items.unshift({
    command: normalized,
    hostId: host.id,
    hostLabel: host.label || host.hostname,
    at: Math.floor(Date.now() / 1000),
  });
  write(items);
}
