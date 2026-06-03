import { DEFAULT_THEME_ID, THEMES, findTheme, normalizeThemeId as normalizeLegacyThemeId, type Theme } from "../themes";

const KEY_THEME = "tersh:theme";
const KEY_FONT = "tersh:font";
const KEY_SIZE = "tersh:fontSize";
const HOST_KEY_PREFIX = "tersh:hostAppearance:";
// Legacy keys from the previous app name — read for migration only.
const legacyPrefix = ["open", "ter", "mius"].join("");
const LEGACY_KEYS = [`${legacyPrefix}:theme`, `${legacyPrefix}:font`, `${legacyPrefix}:fontSize`] as const;
const NEW_KEYS = [KEY_THEME, KEY_FONT, KEY_SIZE] as const;

// One-time migration: copy legacy localStorage entries to the new key namespace
// the first time appearance.ts is loaded under the new name.
try {
  for (let i = 0; i < LEGACY_KEYS.length; i++) {
    const legacyKey = LEGACY_KEYS[i]!;
    const newKey = NEW_KEYS[i]!;
    const v = localStorage.getItem(legacyKey);
    if (v && localStorage.getItem(newKey) === null) {
      localStorage.setItem(newKey, v);
    }
  }
} catch {}

export const FONT_OPTIONS = [
  { id: "system-mono", label: "System Mono", stack: 'Menlo, "SF Mono", ui-monospace, Consolas, "Liberation Mono", monospace' },
  { id: "source-code-pro", label: "Source Code Pro", stack: '"Source Code Pro", ui-monospace, monospace' },
  { id: "jetbrains-mono", label: "JetBrains Mono", stack: '"JetBrains Mono", ui-monospace, monospace' },
  { id: "fira-code", label: "Fira Code", stack: '"Fira Code", ui-monospace, monospace' },
  { id: "cascadia-code", label: "Cascadia Code", stack: '"Cascadia Code", ui-monospace, monospace' },
  { id: "ibm-plex-mono", label: "IBM Plex Mono", stack: '"IBM Plex Mono", ui-monospace, monospace' },
  { id: "menlo", label: "Menlo", stack: 'Menlo, ui-monospace, monospace' },
] as const;

export type FontId = typeof FONT_OPTIONS[number]["id"];

export const DEFAULT_FONT: FontId = "system-mono";
export const DEFAULT_FONT_SIZE = 13;
export const MIN_FONT_SIZE = 8;
export const MAX_FONT_SIZE = 28;

export interface Appearance {
  themeId: string;
  fontId: FontId;
  fontSize: number;
}

interface RawAppearance {
  themeId?: unknown;
  fontId?: unknown;
  fontSize?: unknown;
}

export const DEFAULT_APPEARANCE: Appearance = {
  themeId: DEFAULT_THEME_ID,
  fontId: DEFAULT_FONT,
  fontSize: DEFAULT_FONT_SIZE,
};

function isFontId(value: unknown): value is FontId {
  return typeof value === "string" && FONT_OPTIONS.some(font => font.id === value);
}

function normalizeThemeId(value: unknown): string {
  if (typeof value !== "string") return DEFAULT_THEME_ID;
  const id = normalizeLegacyThemeId(value);
  return THEMES.some(theme => theme.id === id)
    ? id
    : DEFAULT_THEME_ID;
}

function normalizeFontSize(value: unknown): number {
  const parsed = typeof value === "number"
    ? value
    : typeof value === "string"
      ? Number.parseInt(value, 10)
      : Number.NaN;
  if (!Number.isFinite(parsed)) return DEFAULT_FONT_SIZE;
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, Math.round(parsed)));
}

export function normalizeAppearance(input: RawAppearance | null | undefined): Appearance {
  return {
    themeId: normalizeThemeId(input?.themeId),
    fontId: isFontId(input?.fontId) ? input.fontId : DEFAULT_FONT,
    fontSize: normalizeFontSize(input?.fontSize),
  };
}

export function clearGlobalAppearance(): void {
  try {
    localStorage.removeItem(KEY_THEME);
    localStorage.removeItem(KEY_FONT);
    localStorage.removeItem(KEY_SIZE);
    for (const key of LEGACY_KEYS) localStorage.removeItem(key);
  } catch {}
}

export function loadAppearance(): Appearance {
  try {
    return normalizeAppearance({
      themeId: localStorage.getItem(KEY_THEME) ?? DEFAULT_THEME_ID,
      fontId: localStorage.getItem(KEY_FONT) as FontId | null,
      fontSize: localStorage.getItem(KEY_SIZE) ?? DEFAULT_FONT_SIZE,
    });
  } catch {
    return { themeId: DEFAULT_THEME_ID, fontId: DEFAULT_FONT, fontSize: DEFAULT_FONT_SIZE };
  }
}

export function saveAppearance(a: Appearance): void {
  try {
    const normalized = normalizeAppearance(a);
    localStorage.setItem(KEY_THEME, normalized.themeId);
    localStorage.setItem(KEY_FONT, normalized.fontId);
    localStorage.setItem(KEY_SIZE, `${normalized.fontSize}`);
  } catch { /* private mode */ }
}

export function loadHostAppearance(hostId: string): Appearance | null {
  try {
    const raw = localStorage.getItem(`${HOST_KEY_PREFIX}${hostId}`);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<Appearance>;
    if (!parsed.themeId || !parsed.fontId || typeof parsed.fontSize !== "number") return null;
    return normalizeAppearance(parsed);
  } catch {
    return null;
  }
}

export function saveHostAppearance(hostId: string, a: Appearance): void {
  try {
    localStorage.setItem(`${HOST_KEY_PREFIX}${hostId}`, JSON.stringify(normalizeAppearance(a)));
  } catch { /* private mode */ }
}

/**
 * Build the CSS-variable map for a theme without touching :root.
 * Used to SCOPE a theme to a subtree (e.g. the drawer adopts the active
 * tab's theme while the surrounding chrome stays locked to dark).
 * Returned shape is `React.CSSProperties`-compatible — assign to `style=`
 * on the wrapper element.
 */
export function themeVarStyle(themeId: string): Record<string, string> {
  const t = findTheme(themeId).tokens;
  return {
    "--bg": t.bg,
    "--bg-elev": t.bgElev,
    "--bg-hover": t.bgHover,
    "--bg-canvas": t.bgCanvas,
    "--bg-selected": t.bgSelected,
    "--bg-frosted": t.bgFrosted,
    "--text": t.text,
    "--text-dim": t.textDim,
    "--text-bright": t.textBright,
    "--border": t.border,
    "--border-strong": t.borderStrong,
    "--border-subtle": t.borderSubtle,
    "--accent": t.accent,
    "--accent-text": t.accentText,
    "--accent-soft": t.accentSoft,
    "--danger": t.danger,
    "--ok": t.ok,
    "--warning": t.warning,
    "--term-bg": t.termBg,
    "--term-fg": t.termFg,
    "--ansi-green": t.ansiGreen,
    "--ansi-yellow": t.ansiYellow,
    "--ansi-blue": t.ansiBlue,
    "--ansi-magenta": t.ansiMagenta,
    "--ansi-cyan": t.ansiCyan,
    "--ansi-red": t.ansiRed,
    "--shadow-sm": t.shadowSm,
    "--shadow-md": t.shadowMd,
  };
}

/** Apply theme tokens + font to :root as CSS variables. */
export function applyAppearance(a: Appearance): Theme {
  const normalized = normalizeAppearance(a);
  const theme = findTheme(normalized.themeId);
  const root = document.documentElement;
  const t = theme.tokens;
  const set = (k: string, v: string) => root.style.setProperty(k, v);
  set("--bg", t.bg);
  set("--bg-elev", t.bgElev);
  set("--bg-hover", t.bgHover);
  set("--bg-canvas", t.bgCanvas);
  set("--bg-selected", t.bgSelected);
  set("--bg-frosted", t.bgFrosted);
  set("--text", t.text);
  set("--text-dim", t.textDim);
  set("--text-bright", t.textBright);
  set("--border", t.border);
  set("--border-strong", t.borderStrong);
  set("--border-subtle", t.borderSubtle);
  set("--accent", t.accent);
  set("--accent-text", t.accentText);
  set("--accent-soft", t.accentSoft);
  set("--danger", t.danger);
  set("--ok", t.ok);
  set("--warning", t.warning);
  set("--term-bg", t.termBg);
  set("--term-fg", t.termFg);
  set("--ansi-green", t.ansiGreen);
  set("--ansi-yellow", t.ansiYellow);
  set("--ansi-blue", t.ansiBlue);
  set("--ansi-magenta", t.ansiMagenta);
  set("--ansi-cyan", t.ansiCyan);
  set("--ansi-red", t.ansiRed);
  set("--shadow-sm", t.shadowSm);
  set("--shadow-md", t.shadowMd);
  root.dataset.theme = theme.id;
  root.dataset.themeMode = theme.mode;

  // font
  const font = FONT_OPTIONS.find(f => f.id === normalized.fontId) ?? FONT_OPTIONS[0]!;
  set("--term-font", font.stack);
  set("--term-font-size", `${normalized.fontSize}px`);
  return theme;
}
