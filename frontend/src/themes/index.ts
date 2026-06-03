export interface XtermPalette {
  background: string; foreground: string;
  cursor: string; cursorAccent: string;
  selectionBackground: string;
  black: string; red: string; green: string; yellow: string;
  blue: string; magenta: string; cyan: string; white: string;
  brightBlack: string; brightRed: string; brightGreen: string; brightYellow: string;
  brightBlue: string; brightMagenta: string; brightCyan: string; brightWhite: string;
}

export interface UiTokens {
  bg: string; bgElev: string; bgHover: string; bgCanvas: string; bgSelected: string;
  bgFrosted: string;
  text: string; textDim: string; textBright: string;
  border: string; borderStrong: string; borderSubtle: string;
  accent: string; accentText: string; accentSoft: string;
  danger: string; ok: string; warning: string;
  termBg: string; termFg: string;
  ansiGreen: string; ansiYellow: string; ansiBlue: string;
  ansiMagenta: string; ansiCyan: string; ansiRed: string;
  shadowSm: string; shadowMd: string;
}

export interface Theme {
  id: string;
  name: string;
  mode: "dark" | "light";
  tokens: UiTokens;
  xterm: XtermPalette;
}

const mkXterm = (t: UiTokens, extra: Partial<XtermPalette> = {}): XtermPalette => ({
  background: t.termBg,
  foreground: t.termFg,
  cursor: t.termFg,
  cursorAccent: t.termBg,
  selectionBackground: t.bgHover,
  black: t.termBg,
  red: t.ansiRed,
  green: t.ansiGreen,
  yellow: t.ansiYellow,
  blue: t.ansiBlue,
  magenta: t.ansiMagenta,
  cyan: t.ansiCyan,
  // ANSI white is used by plenty of shells/tools for ordinary output. Mapping
  // it to UI-muted text made terminal output look disabled/dim on dark themes.
  // Keep "white" equal to the terminal foreground; reserve brightBlack for a
  // readable-but-dim grey.
  white: t.termFg,
  brightBlack: t.textDim,
  brightRed: t.ansiRed,
  brightGreen: t.ansiGreen,
  brightYellow: t.ansiYellow,
  brightBlue: t.ansiBlue,
  brightMagenta: t.ansiMagenta,
  brightCyan: t.ansiCyan,
  brightWhite: t.textBright,
  ...extra,
});

const dark = (id: string, name: string, tokens: UiTokens, xtermOverrides: Partial<XtermPalette> = {}): Theme =>
  ({ id, name, mode: "dark", tokens, xterm: mkXterm(tokens, xtermOverrides) });

const light = (id: string, name: string, tokens: UiTokens, xtermOverrides: Partial<XtermPalette> = {}): Theme =>
  ({ id, name, mode: "light", tokens, xterm: mkXterm(tokens, xtermOverrides) });

const legacyThemePrefix = ["ter", "mius"].join("");
export const LEGACY_THEME_IDS: Record<string, string> = {
  [`${legacyThemePrefix}-dark`]: "tersh-dark",
  [`${legacyThemePrefix}-light`]: "tersh-light",
};

export const normalizeThemeId = (id: string): string => LEGACY_THEME_IDS[id] ?? id;

export const THEMES: Theme[] = [
  dark("tersh-dark", "Tersh Dark", {
    bg: "#202336", bgElev: "#2a2d40", bgHover: "#34384c", bgCanvas: "#1b1e2f", bgSelected: "#6d86ff24", bgFrosted: "rgba(42,45,64,.88)",
    text: "#f6f7ff", textDim: "#aeb4c8", textBright: "#ffffff",
    border: "#35394f", borderStrong: "#4a5068", borderSubtle: "#2b2f43",
    accent: "#7fa2ff", accentText: "#101322", accentSoft: "#7fa2ff30",
    danger: "#ff6f8f", ok: "#72d49b", warning: "#e8c46d",
    // termFg is the xterm foreground (default un-coloured shell output).
    // Previously #d8dbea — same purple family as termBg, looked washed.
    // Bumped to near-pure-white so unannotated text reads clearly on the
    // dark navy. Programs using explicit ANSI colours still use the palette
    // below (cyan, green, etc.).
    termBg: "#202336", termFg: "#f6f7ff",
    ansiGreen: "#72d49b", ansiYellow: "#e8c46d", ansiBlue: "#7fa2ff",
    ansiMagenta: "#c8a4ff", ansiCyan: "#70d5e8", ansiRed: "#ff7f93",
    shadowSm: "0 1px 0 rgba(0,0,0,.32)", shadowMd: "0 6px 18px rgba(0,0,0,.28)",
  }),
  light("tersh-light", "Tersh Light", {
    bg: "#ffffff", bgElev: "#f6f7f9", bgHover: "#eef0f3", bgCanvas: "#eef1f4", bgSelected: "#0969da12", bgFrosted: "rgba(255,255,255,.85)",
    text: "#1f2328", textDim: "#59636e", textBright: "#0a0c10",
    border: "#d8dee4", borderStrong: "#bac1c9", borderSubtle: "#e6ebf0",
    accent: "#0969da", accentText: "#ffffff", accentSoft: "#0969da22",
    danger: "#cf222e", ok: "#1a7f37", warning: "#9a6700",
    termBg: "#ffffff", termFg: "#1f2328",
    ansiGreen: "#1a7f37", ansiYellow: "#9a6700", ansiBlue: "#0969da",
    ansiMagenta: "#8250df", ansiCyan: "#1b7c83", ansiRed: "#cf222e",
    shadowSm: "0 1px 0 rgba(15,20,30,.04)", shadowMd: "0 2px 12px rgba(15,20,30,.08)",
  }),
  dark("kanagawa-wave", "Kanagawa Wave", {
    bg: "#1f1f28", bgElev: "#2a2a37", bgHover: "#363646", bgCanvas: "#16161d", bgSelected: "#7e9cd81f", bgFrosted: "rgba(31,31,40,.85)",
    text: "#dcd7ba", textDim: "#727169", textBright: "#c8c093",
    border: "#363646", borderStrong: "#54546d", borderSubtle: "#2a2a37",
    accent: "#7e9cd8", accentText: "#1f1f28", accentSoft: "#7e9cd833",
    danger: "#e82424", ok: "#98bb6c", warning: "#dca561",
    termBg: "#1f1f28", termFg: "#dcd7ba",
    ansiGreen: "#98bb6c", ansiYellow: "#e6c384", ansiBlue: "#7fb4ca",
    ansiMagenta: "#957fb8", ansiCyan: "#7aa89f", ansiRed: "#c34043",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("kanagawa-dragon", "Kanagawa Dragon", {
    bg: "#181616", bgElev: "#1d1c19", bgHover: "#2a2826", bgCanvas: "#0d0c0c", bgSelected: "#6585941f", bgFrosted: "rgba(24,22,22,.85)",
    text: "#c5c9c5", textDim: "#737c73", textBright: "#e5e5e5",
    border: "#2a2826", borderStrong: "#3a3a3a", borderSubtle: "#1d1c19",
    accent: "#658594", accentText: "#181616", accentSoft: "#65859433",
    danger: "#c4746e", ok: "#8a9a7b", warning: "#c4b28a",
    termBg: "#181616", termFg: "#c5c9c5",
    ansiGreen: "#8a9a7b", ansiYellow: "#c4b28a", ansiBlue: "#8ba4b0",
    ansiMagenta: "#a292a3", ansiCyan: "#8ea4a2", ansiRed: "#c4746e",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  light("kanagawa-lotus", "Kanagawa Lotus", {
    bg: "#f2ecbc", bgElev: "#ece5b1", bgHover: "#e2d99c", bgCanvas: "#dcd2a0", bgSelected: "#4d699b1f", bgFrosted: "rgba(242,236,188,.85)",
    text: "#545464", textDim: "#8a8980", textBright: "#43436c",
    border: "#cccac2", borderStrong: "#a09cac", borderSubtle: "#d8d2af",
    accent: "#4d699b", accentText: "#f2ecbc", accentSoft: "#4d699b22",
    danger: "#c84053", ok: "#6f894e", warning: "#cc6d00",
    termBg: "#f2ecbc", termFg: "#545464",
    ansiGreen: "#6f894e", ansiYellow: "#77713f", ansiBlue: "#4d699b",
    ansiMagenta: "#b35b79", ansiCyan: "#597b75", ansiRed: "#c84053",
    shadowSm: "0 1px 0 rgba(0,0,0,.05)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("flexoki-dark", "Flexoki Dark", {
    bg: "#100f0f", bgElev: "#1c1b1a", bgHover: "#282726", bgCanvas: "#0a0908", bgSelected: "#4385be1f", bgFrosted: "rgba(28,27,26,.85)",
    text: "#cecdc3", textDim: "#878580", textBright: "#fffcf0",
    border: "#282726", borderStrong: "#403e3c", borderSubtle: "#1c1b1a",
    accent: "#4385be", accentText: "#100f0f", accentSoft: "#4385be33",
    danger: "#d14d41", ok: "#879a39", warning: "#d0a215",
    termBg: "#100f0f", termFg: "#cecdc3",
    ansiGreen: "#879a39", ansiYellow: "#d0a215", ansiBlue: "#4385be",
    ansiMagenta: "#ce5d97", ansiCyan: "#3aa99f", ansiRed: "#d14d41",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  light("flexoki-light", "Flexoki Light", {
    bg: "#fffcf0", bgElev: "#f2f0e5", bgHover: "#e6e4d9", bgCanvas: "#ece9d8", bgSelected: "#205ea611", bgFrosted: "rgba(255,252,240,.85)",
    text: "#100f0f", textDim: "#6f6e69", textBright: "#000000",
    border: "#e6e4d9", borderStrong: "#cecdc3", borderSubtle: "#ece9d8",
    accent: "#205ea6", accentText: "#fffcf0", accentSoft: "#205ea622",
    danger: "#af3029", ok: "#66800b", warning: "#ad8301",
    termBg: "#fffcf0", termFg: "#100f0f",
    ansiGreen: "#66800b", ansiYellow: "#ad8301", ansiBlue: "#205ea6",
    ansiMagenta: "#a02f6f", ansiCyan: "#24837b", ansiRed: "#af3029",
    shadowSm: "0 1px 0 rgba(0,0,0,.04)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("catppuccin-mocha", "Catppuccin Mocha", {
    bg: "#1e1e2e", bgElev: "#181825", bgHover: "#313244", bgCanvas: "#11111b", bgSelected: "#89b4fa1f", bgFrosted: "rgba(24,24,37,.85)",
    text: "#cdd6f4", textDim: "#6c7086", textBright: "#f5e0dc",
    border: "#313244", borderStrong: "#45475a", borderSubtle: "#1e1e2e",
    accent: "#89b4fa", accentText: "#1e1e2e", accentSoft: "#89b4fa33",
    danger: "#f38ba8", ok: "#a6e3a1", warning: "#f9e2af",
    termBg: "#1e1e2e", termFg: "#cdd6f4",
    ansiGreen: "#a6e3a1", ansiYellow: "#f9e2af", ansiBlue: "#89b4fa",
    ansiMagenta: "#f5c2e7", ansiCyan: "#94e2d5", ansiRed: "#f38ba8",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("tokyo-night", "Tokyo Night", {
    bg: "#1a1b26", bgElev: "#16161e", bgHover: "#24283b", bgCanvas: "#0f0f17", bgSelected: "#7aa2f71f", bgFrosted: "rgba(22,22,30,.85)",
    text: "#c0caf5", textDim: "#565f89", textBright: "#a9b1d6",
    border: "#24283b", borderStrong: "#414868", borderSubtle: "#1a1b26",
    accent: "#7aa2f7", accentText: "#1a1b26", accentSoft: "#7aa2f733",
    danger: "#f7768e", ok: "#9ece6a", warning: "#e0af68",
    termBg: "#1a1b26", termFg: "#c0caf5",
    ansiGreen: "#9ece6a", ansiYellow: "#e0af68", ansiBlue: "#7aa2f7",
    ansiMagenta: "#bb9af7", ansiCyan: "#7dcfff", ansiRed: "#f7768e",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("everforest-dark", "Everforest Dark", {
    bg: "#2d353b", bgElev: "#343f44", bgHover: "#3d484d", bgCanvas: "#232a2e", bgSelected: "#a7c0801f", bgFrosted: "rgba(52,63,68,.85)",
    text: "#d3c6aa", textDim: "#859289", textBright: "#e8e3c8",
    border: "#3d484d", borderStrong: "#475258", borderSubtle: "#343f44",
    accent: "#a7c080", accentText: "#2d353b", accentSoft: "#a7c08033",
    danger: "#e67e80", ok: "#a7c080", warning: "#dbbc7f",
    termBg: "#2d353b", termFg: "#d3c6aa",
    ansiGreen: "#a7c080", ansiYellow: "#dbbc7f", ansiBlue: "#7fbbb3",
    ansiMagenta: "#d699b6", ansiCyan: "#83c092", ansiRed: "#e67e80",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("solarized-dark", "Solarized Dark", {
    bg: "#002b36", bgElev: "#073642", bgHover: "#0a4452", bgCanvas: "#001f29", bgSelected: "#268bd21f", bgFrosted: "rgba(7,54,66,.85)",
    text: "#839496", textDim: "#586e75", textBright: "#eee8d5",
    border: "#073642", borderStrong: "#586e75", borderSubtle: "#04303b",
    accent: "#268bd2", accentText: "#002b36", accentSoft: "#268bd233",
    danger: "#dc322f", ok: "#859900", warning: "#b58900",
    termBg: "#002b36", termFg: "#839496",
    ansiGreen: "#859900", ansiYellow: "#b58900", ansiBlue: "#268bd2",
    ansiMagenta: "#d33682", ansiCyan: "#2aa198", ansiRed: "#dc322f",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  // The three "hacker" themes split bg/term-bg deliberately: the chrome
  // surfaces (bg/bgElev/bgHover) carry a saturated theme hue so the drawer
  // visibly tints red/blue/green when scoped, while the terminal pane
  // (termBg) stays nearly black to preserve the classic "hacker terminal"
  // aesthetic. text/textDim are softened to warm coral / sky / mint so they
  // read as themed-but-not-eye-melting in chrome surfaces.
  dark("hacker-green", "Hacker Green", {
    bg: "#031805", bgElev: "#08240c", bgHover: "#103817", bgCanvas: "#010c03", bgSelected: "#00ff4116", bgFrosted: "rgba(8,36,12,.9)",
    text: "#00ff41", textDim: "#15963a", textBright: "#b8ffc8",
    border: "#123918", borderStrong: "#1f7030", borderSubtle: "#08240c",
    accent: "#00ff41", accentText: "#061a0a", accentSoft: "#00ff4133",
    danger: "#ff5e5e", ok: "#00ff41", warning: "#ffd166",
    termBg: "#031805", termFg: "#00ff41",
    ansiGreen: "#00ff41", ansiYellow: "#ffff66", ansiBlue: "#66ccff",
    ansiMagenta: "#ff7ad6", ansiCyan: "#66ffff", ansiRed: "#ff5e5e",
    shadowSm: "0 1px 0 rgba(0,255,65,.14)", shadowMd: "0 2px 16px rgba(0,255,65,.18)",
  }),
  dark("hacker-blue", "Hacker Blue", {
    bg: "#061326", bgElev: "#0a1d3a", bgHover: "#102e5d", bgCanvas: "#020a18", bgSelected: "#168bff18", bgFrosted: "rgba(10,29,58,.9)",
    text: "#168bff", textDim: "#2f6eaa", textBright: "#b8dcff",
    border: "#12335f", borderStrong: "#1f5fa8", borderSubtle: "#0a1d3a",
    accent: "#168bff", accentText: "#020a18", accentSoft: "#168bff33",
    danger: "#ff4f78", ok: "#168bff", warning: "#ffd166",
    termBg: "#061326", termFg: "#168bff",
    ansiGreen: "#168bff", ansiYellow: "#ffd166", ansiBlue: "#168bff",
    ansiMagenta: "#a96cff", ansiCyan: "#47d7ff", ansiRed: "#ff4f78",
    shadowSm: "0 1px 0 rgba(22,139,255,.14)", shadowMd: "0 2px 16px rgba(22,139,255,.18)",
  }),
  dark("hacker-red", "Hacker Red", {
    bg: "#250707", bgElev: "#311010", bgHover: "#431818", bgCanvas: "#1b0303", bgSelected: "#ff6b6b18", bgFrosted: "rgba(49,16,16,.9)",
    text: "#ff2424", textDim: "#b83a36", textBright: "#ffb8b2",
    border: "#4b1a18", borderStrong: "#7a2b27", borderSubtle: "#33110f",
    accent: "#ff2424", accentText: "#1b0303", accentSoft: "#ff242430",
    danger: "#ff2424", ok: "#ff2424", warning: "#d98b50",
    termBg: "#250707", termFg: "#ff2424",
    ansiGreen: "#ff2424", ansiYellow: "#d98b50", ansiBlue: "#c15a56",
    ansiMagenta: "#d0619f", ansiCyan: "#bd7770", ansiRed: "#ff2424",
    shadowSm: "0 1px 0 rgba(217,74,66,.12)", shadowMd: "0 2px 12px rgba(217,74,66,.14)",
  }),
  dark("hacker-cyan", "Hacker Cyan", {
    bg: "#031718", bgElev: "#082527", bgHover: "#103b3f", bgCanvas: "#010c0d", bgSelected: "#00f5ff18", bgFrosted: "rgba(8,37,39,.9)",
    text: "#00f5ff", textDim: "#1e9aa0", textBright: "#b8fbff",
    border: "#123b3e", borderStrong: "#1c7076", borderSubtle: "#082527",
    accent: "#00f5ff", accentText: "#010c0d", accentSoft: "#00f5ff33",
    danger: "#ff4f78", ok: "#00f5ff", warning: "#ffd166",
    termBg: "#031718", termFg: "#00f5ff",
    ansiGreen: "#00f5ff", ansiYellow: "#ffd166", ansiBlue: "#42a5ff",
    ansiMagenta: "#ff4fd8", ansiCyan: "#00f5ff", ansiRed: "#ff4f78",
    shadowSm: "0 1px 0 rgba(0,245,255,.14)", shadowMd: "0 2px 16px rgba(0,245,255,.18)",
  }),
  dark("hacker-purple", "Hacker Purple", {
    bg: "#13081f", bgElev: "#1f0d32", bgHover: "#32164f", bgCanvas: "#0a0312", bgSelected: "#a855ff18", bgFrosted: "rgba(31,13,50,.9)",
    text: "#a855ff", textDim: "#7243a4", textBright: "#e2c7ff",
    border: "#33184f", borderStrong: "#633197", borderSubtle: "#1f0d32",
    accent: "#a855ff", accentText: "#0a0312", accentSoft: "#a855ff33",
    danger: "#ff4f78", ok: "#a855ff", warning: "#ffd166",
    termBg: "#13081f", termFg: "#a855ff",
    ansiGreen: "#a855ff", ansiYellow: "#ffd166", ansiBlue: "#7aa7ff",
    ansiMagenta: "#d46bff", ansiCyan: "#66f6ff", ansiRed: "#ff4f78",
    shadowSm: "0 1px 0 rgba(168,85,255,.14)", shadowMd: "0 2px 16px rgba(168,85,255,.18)",
  }),
  dark("hacker-pink", "Hacker Pink", {
    bg: "#200716", bgElev: "#320b22", bgHover: "#501236", bgCanvas: "#12030b", bgSelected: "#ff2db218", bgFrosted: "rgba(50,11,34,.9)",
    text: "#ff2db2", textDim: "#a9347e", textBright: "#ffc2eb",
    border: "#501338", borderStrong: "#93306d", borderSubtle: "#320b22",
    accent: "#ff2db2", accentText: "#12030b", accentSoft: "#ff2db233",
    danger: "#ff4f78", ok: "#ff2db2", warning: "#ffd166",
    termBg: "#200716", termFg: "#ff2db2",
    ansiGreen: "#ff2db2", ansiYellow: "#ffd166", ansiBlue: "#42d9ff",
    ansiMagenta: "#ff2db2", ansiCyan: "#66f6ff", ansiRed: "#ff4f78",
    shadowSm: "0 1px 0 rgba(255,45,178,.14)", shadowMd: "0 2px 16px rgba(255,45,178,.18)",
  }),
  dark("hacker-amber", "Hacker Amber", {
    bg: "#1c1002", bgElev: "#2b1805", bgHover: "#46280a", bgCanvas: "#100900", bgSelected: "#ffb00018", bgFrosted: "rgba(43,24,5,.9)",
    text: "#ffb000", textDim: "#a8781a", textBright: "#ffe1a1",
    border: "#4c2c0d", borderStrong: "#8a5718", borderSubtle: "#2b1805",
    accent: "#ffb000", accentText: "#100900", accentSoft: "#ffb00033",
    danger: "#ff4f4f", ok: "#ffb000", warning: "#ffd166",
    termBg: "#1c1002", termFg: "#ffb000",
    ansiGreen: "#ffb000", ansiYellow: "#ffd166", ansiBlue: "#59c2ff",
    ansiMagenta: "#ff7ad6", ansiCyan: "#66ffff", ansiRed: "#ff4f4f",
    shadowSm: "0 1px 0 rgba(255,176,0,.14)", shadowMd: "0 2px 16px rgba(255,176,0,.18)",
  }),
  light("solarized-light", "Solarized Light", {
    bg: "#fdf6e3", bgElev: "#eee8d5", bgHover: "#e4ddc8", bgCanvas: "#f5eed7", bgSelected: "#268bd222", bgFrosted: "rgba(253,246,227,.85)",
    text: "#586e75", textDim: "#93a1a1", textBright: "#002b36",
    border: "#eee8d5", borderStrong: "#93a1a1", borderSubtle: "#f0eada",
    accent: "#268bd2", accentText: "#fdf6e3", accentSoft: "#268bd222",
    danger: "#dc322f", ok: "#859900", warning: "#b58900",
    termBg: "#fdf6e3", termFg: "#586e75",
    ansiGreen: "#859900", ansiYellow: "#b58900", ansiBlue: "#268bd2",
    ansiMagenta: "#d33682", ansiCyan: "#2aa198", ansiRed: "#dc322f",
    shadowSm: "0 1px 0 rgba(0,0,0,.04)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("nord", "Nord", {
    bg: "#2e3440", bgElev: "#3b4252", bgHover: "#434c5e", bgCanvas: "#242933", bgSelected: "#88c0d01f", bgFrosted: "rgba(59,66,82,.85)",
    text: "#d8dee9", textDim: "#7b889c", textBright: "#eceff4",
    border: "#3b4252", borderStrong: "#4c566a", borderSubtle: "#353b48",
    accent: "#88c0d0", accentText: "#2e3440", accentSoft: "#88c0d033",
    danger: "#bf616a", ok: "#a3be8c", warning: "#ebcb8b",
    termBg: "#2e3440", termFg: "#eceff4",
    ansiGreen: "#a3be8c", ansiYellow: "#ebcb8b", ansiBlue: "#81a1c1",
    ansiMagenta: "#b48ead", ansiCyan: "#8fbcbb", ansiRed: "#bf616a",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  light("nord-light", "Nord Light", {
    bg: "#eceff4", bgElev: "#e5e9f0", bgHover: "#d8dee9", bgCanvas: "#f5f7fa", bgSelected: "#5e81ac1f", bgFrosted: "rgba(236,239,244,.85)",
    text: "#2e3440", textDim: "#6c7a96", textBright: "#1c2128",
    border: "#d8dee9", borderStrong: "#aab2bf", borderSubtle: "#e5e9f0",
    accent: "#5e81ac", accentText: "#eceff4", accentSoft: "#5e81ac22",
    danger: "#bf616a", ok: "#a3be8c", warning: "#d08770",
    termBg: "#eceff4", termFg: "#2e3440",
    ansiGreen: "#7d9367", ansiYellow: "#b78a52", ansiBlue: "#5e81ac",
    ansiMagenta: "#a07798", ansiCyan: "#6a9591", ansiRed: "#bf616a",
    shadowSm: "0 1px 0 rgba(0,0,0,.04)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("gruvbox-dark", "Gruvbox Dark", {
    bg: "#282828", bgElev: "#32302f", bgHover: "#3c3836", bgCanvas: "#1d2021", bgSelected: "#fabd2f1f", bgFrosted: "rgba(50,48,47,.85)",
    text: "#ebdbb2", textDim: "#a89984", textBright: "#fbf1c7",
    border: "#3c3836", borderStrong: "#504945", borderSubtle: "#32302f",
    accent: "#fabd2f", accentText: "#282828", accentSoft: "#fabd2f33",
    danger: "#fb4934", ok: "#b8bb26", warning: "#fe8019",
    termBg: "#282828", termFg: "#ebdbb2",
    ansiGreen: "#b8bb26", ansiYellow: "#fabd2f", ansiBlue: "#83a598",
    ansiMagenta: "#d3869b", ansiCyan: "#8ec07c", ansiRed: "#fb4934",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("monokai-pro", "Monokai Pro", {
    bg: "#2d2a2e", bgElev: "#221f22", bgHover: "#403e41", bgCanvas: "#19181a", bgSelected: "#ffd8661f", bgFrosted: "rgba(34,31,34,.85)",
    text: "#fcfcfa", textDim: "#727072", textBright: "#ffffff",
    border: "#403e41", borderStrong: "#5b595c", borderSubtle: "#2a272a",
    accent: "#ffd866", accentText: "#2d2a2e", accentSoft: "#ffd86633",
    danger: "#ff6188", ok: "#a9dc76", warning: "#fc9867",
    termBg: "#2d2a2e", termFg: "#fcfcfa",
    ansiGreen: "#a9dc76", ansiYellow: "#ffd866", ansiBlue: "#fc9867",
    ansiMagenta: "#ab9df2", ansiCyan: "#78dce8", ansiRed: "#ff6188",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("dracula", "Dracula", {
    bg: "#282a36", bgElev: "#21222c", bgHover: "#44475a", bgCanvas: "#191a21", bgSelected: "#bd93f91f", bgFrosted: "rgba(33,34,44,.85)",
    text: "#f8f8f2", textDim: "#6272a4", textBright: "#ffffff",
    border: "#44475a", borderStrong: "#6272a4", borderSubtle: "#2e313e",
    accent: "#bd93f9", accentText: "#282a36", accentSoft: "#bd93f933",
    danger: "#ff5555", ok: "#50fa7b", warning: "#f1fa8c",
    termBg: "#282a36", termFg: "#f8f8f2",
    ansiGreen: "#50fa7b", ansiYellow: "#f1fa8c", ansiBlue: "#bd93f9",
    ansiMagenta: "#ff79c6", ansiCyan: "#8be9fd", ansiRed: "#ff5555",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("one-dark", "One Dark", {
    bg: "#282c34", bgElev: "#21252b", bgHover: "#3e4451", bgCanvas: "#1e2229", bgSelected: "#61afef1f", bgFrosted: "rgba(33,37,43,.85)",
    text: "#abb2bf", textDim: "#5c6370", textBright: "#ffffff",
    border: "#3e4451", borderStrong: "#4b5263", borderSubtle: "#262a31",
    accent: "#61afef", accentText: "#282c34", accentSoft: "#61afef33",
    danger: "#e06c75", ok: "#98c379", warning: "#e5c07b",
    termBg: "#282c34", termFg: "#abb2bf",
    ansiGreen: "#98c379", ansiYellow: "#e5c07b", ansiBlue: "#61afef",
    ansiMagenta: "#c678dd", ansiCyan: "#56b6c2", ansiRed: "#e06c75",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("ayu-dark", "Ayu Dark", {
    bg: "#0b0e14", bgElev: "#11151c", bgHover: "#1c212a", bgCanvas: "#07090d", bgSelected: "#e6b4501f", bgFrosted: "rgba(17,21,28,.85)",
    text: "#bfbdb6", textDim: "#5c6773", textBright: "#ffffff",
    border: "#1c212a", borderStrong: "#3e4148", borderSubtle: "#11151c",
    accent: "#e6b450", accentText: "#0b0e14", accentSoft: "#e6b45033",
    danger: "#f07178", ok: "#aad94c", warning: "#ffb454",
    termBg: "#0b0e14", termFg: "#bfbdb6",
    ansiGreen: "#aad94c", ansiYellow: "#ffb454", ansiBlue: "#59c2ff",
    ansiMagenta: "#d2a6ff", ansiCyan: "#95e6cb", ansiRed: "#f07178",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  light("ayu-light", "Ayu Light", {
    bg: "#fcfcfc", bgElev: "#f5f5f5", bgHover: "#eaeaea", bgCanvas: "#ffffff", bgSelected: "#ff994022", bgFrosted: "rgba(252,252,252,.85)",
    text: "#5c6773", textDim: "#828c99", textBright: "#1a1f29",
    border: "#eaeaea", borderStrong: "#bcc4cc", borderSubtle: "#f0f0f0",
    accent: "#ff9940", accentText: "#1a1f29", accentSoft: "#ff994033",
    danger: "#f07171", ok: "#86b300", warning: "#f2ae49",
    termBg: "#fcfcfc", termFg: "#5c6773",
    ansiGreen: "#86b300", ansiYellow: "#f2ae49", ansiBlue: "#36a3d9",
    ansiMagenta: "#a37acc", ansiCyan: "#4cbf99", ansiRed: "#f07171",
    shadowSm: "0 1px 0 rgba(0,0,0,.04)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("rose-pine", "Rosé Pine", {
    bg: "#191724", bgElev: "#1f1d2e", bgHover: "#26233a", bgCanvas: "#16141f", bgSelected: "#c4a7e71f", bgFrosted: "rgba(31,29,46,.85)",
    text: "#e0def4", textDim: "#908caa", textBright: "#f1efff",
    border: "#26233a", borderStrong: "#403d52", borderSubtle: "#1f1d2e",
    accent: "#c4a7e7", accentText: "#191724", accentSoft: "#c4a7e733",
    danger: "#eb6f92", ok: "#9ccfd8", warning: "#f6c177",
    termBg: "#191724", termFg: "#e0def4",
    ansiGreen: "#ebbcba", ansiYellow: "#f6c177", ansiBlue: "#31748f",
    ansiMagenta: "#c4a7e7", ansiCyan: "#9ccfd8", ansiRed: "#eb6f92",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  dark("rose-pine-moon", "Rosé Pine Moon", {
    bg: "#232136", bgElev: "#2a273f", bgHover: "#393552", bgCanvas: "#1e1c30", bgSelected: "#c4a7e71f", bgFrosted: "rgba(42,39,63,.85)",
    text: "#e0def4", textDim: "#908caa", textBright: "#f1efff",
    border: "#393552", borderStrong: "#56526e", borderSubtle: "#2a273f",
    accent: "#c4a7e7", accentText: "#232136", accentSoft: "#c4a7e733",
    danger: "#eb6f92", ok: "#9ccfd8", warning: "#f6c177",
    termBg: "#232136", termFg: "#e0def4",
    ansiGreen: "#ea9a97", ansiYellow: "#f6c177", ansiBlue: "#3e8fb0",
    ansiMagenta: "#c4a7e7", ansiCyan: "#9ccfd8", ansiRed: "#eb6f92",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  light("rose-pine-dawn", "Rosé Pine Dawn", {
    bg: "#faf4ed", bgElev: "#fffaf3", bgHover: "#f2e9e1", bgCanvas: "#fdf8f2", bgSelected: "#907aa922", bgFrosted: "rgba(250,244,237,.85)",
    text: "#575279", textDim: "#9893a5", textBright: "#393552",
    border: "#f2e9e1", borderStrong: "#dfdad9", borderSubtle: "#f7f1ea",
    accent: "#907aa9", accentText: "#faf4ed", accentSoft: "#907aa922",
    danger: "#b4637a", ok: "#56949f", warning: "#ea9d34",
    termBg: "#faf4ed", termFg: "#575279",
    ansiGreen: "#d7827e", ansiYellow: "#ea9d34", ansiBlue: "#286983",
    ansiMagenta: "#907aa9", ansiCyan: "#56949f", ansiRed: "#b4637a",
    shadowSm: "0 1px 0 rgba(0,0,0,.04)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("night-owl", "Night Owl", {
    bg: "#011627", bgElev: "#0d2236", bgHover: "#1d3b53", bgCanvas: "#001423", bgSelected: "#82aaff1f", bgFrosted: "rgba(13,34,54,.85)",
    text: "#d6deeb", textDim: "#5f7e97", textBright: "#ffffff",
    border: "#1d3b53", borderStrong: "#2e5278", borderSubtle: "#0d2236",
    accent: "#82aaff", accentText: "#011627", accentSoft: "#82aaff33",
    danger: "#ef5350", ok: "#addb67", warning: "#ecc48d",
    termBg: "#011627", termFg: "#d6deeb",
    ansiGreen: "#addb67", ansiYellow: "#ecc48d", ansiBlue: "#82aaff",
    ansiMagenta: "#c792ea", ansiCyan: "#21c7a8", ansiRed: "#ef5350",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 8px rgba(0,0,0,.5)",
  }),
  light("light-owl", "Light Owl", {
    bg: "#fbfbfb", bgElev: "#f0f0f0", bgHover: "#e3e6ea", bgCanvas: "#ffffff", bgSelected: "#4876d622", bgFrosted: "rgba(251,251,251,.85)",
    text: "#403f53", textDim: "#90a4ae", textBright: "#0f1219",
    border: "#e3e6ea", borderStrong: "#bcc4cc", borderSubtle: "#ededed",
    accent: "#4876d6", accentText: "#ffffff", accentSoft: "#4876d622",
    danger: "#d3423e", ok: "#08916a", warning: "#aa5d00",
    termBg: "#fbfbfb", termFg: "#403f53",
    ansiGreen: "#08916a", ansiYellow: "#aa5d00", ansiBlue: "#4876d6",
    ansiMagenta: "#994cc3", ansiCyan: "#2aa298", ansiRed: "#d3423e",
    shadowSm: "0 1px 0 rgba(0,0,0,.04)", shadowMd: "0 2px 12px rgba(0,0,0,.08)",
  }),
  dark("cobalt2", "Cobalt2", {
    bg: "#193549", bgElev: "#15293c", bgHover: "#1f4662", bgCanvas: "#0e2235", bgSelected: "#ffc6001f", bgFrosted: "rgba(21,41,60,.85)",
    text: "#e1efff", textDim: "#7b9cb5", textBright: "#ffffff",
    border: "#1f4662", borderStrong: "#2e5d80", borderSubtle: "#15293c",
    accent: "#ffc600", accentText: "#193549", accentSoft: "#ffc60033",
    danger: "#ff628c", ok: "#3ad900", warning: "#ff9d00",
    termBg: "#193549", termFg: "#ffffff",
    ansiGreen: "#3ad900", ansiYellow: "#ffc600", ansiBlue: "#0088ff",
    ansiMagenta: "#fb94ff", ansiCyan: "#80ffbb", ansiRed: "#ff628c",
    shadowSm: "0 1px 0 rgba(0,0,0,.5)", shadowMd: "0 2px 14px rgba(0,0,0,.45)",
  }),
  dark("cyberpunk", "Cyberpunk", {
    bg: "#23051d", bgElev: "#35102d", bgHover: "#501743", bgCanvas: "#150211", bgSelected: "#ff2bd61f", bgFrosted: "rgba(53,16,45,.88)",
    text: "#ff45d4", textDim: "#b34a9a", textBright: "#ffd1f4",
    border: "#541844", borderStrong: "#923276", borderSubtle: "#35102d",
    accent: "#ff2bd6", accentText: "#150211", accentSoft: "#ff2bd633",
    danger: "#ff365f", ok: "#35ffe6", warning: "#ffe45e",
    termBg: "#23051d", termFg: "#ff45d4",
    ansiGreen: "#35ffe6", ansiYellow: "#ffe45e", ansiBlue: "#35a7ff",
    ansiMagenta: "#ff2bd6", ansiCyan: "#35ffe6", ansiRed: "#ff365f",
    shadowSm: "0 1px 0 rgba(255,43,214,.16)", shadowMd: "0 2px 20px rgba(255,43,214,.24)",
  }),
  dark("cyberpunk-scarlet", "Cyberpunk Scarlet", {
    bg: "#25030b", bgElev: "#3a0714", bgHover: "#5a0d21", bgCanvas: "#150106", bgSelected: "#ff174f1f", bgFrosted: "rgba(58,7,20,.88)",
    text: "#ff3868", textDim: "#b83c55", textBright: "#ffc2cf",
    border: "#5a1224", borderStrong: "#963249", borderSubtle: "#3a0714",
    accent: "#ff174f", accentText: "#150106", accentSoft: "#ff174f33",
    danger: "#ff174f", ok: "#ff3868", warning: "#ff9f1c",
    termBg: "#25030b", termFg: "#ff3868",
    ansiGreen: "#ff3868", ansiYellow: "#ff9f1c", ansiBlue: "#ff6a8f",
    ansiMagenta: "#ff2bd6", ansiCyan: "#ff9ec4", ansiRed: "#ff174f",
    shadowSm: "0 1px 0 rgba(255,23,79,.18)", shadowMd: "0 2px 18px rgba(255,23,79,.22)",
  }),
  dark("aura", "Aura", {
    bg: "#15141b", bgElev: "#1d1c25", bgHover: "#29263c", bgCanvas: "#0f0e15", bgSelected: "#a277ff1f", bgFrosted: "rgba(29,28,37,.85)",
    text: "#edecee", textDim: "#6d6d6d", textBright: "#ffffff",
    border: "#29263c", borderStrong: "#3f3a5a", borderSubtle: "#1d1c25",
    accent: "#a277ff", accentText: "#15141b", accentSoft: "#a277ff33",
    danger: "#ff6767", ok: "#61ffca", warning: "#ffca85",
    termBg: "#15141b", termFg: "#edecee",
    ansiGreen: "#61ffca", ansiYellow: "#ffca85", ansiBlue: "#a277ff",
    ansiMagenta: "#a277ff", ansiCyan: "#82e2ff", ansiRed: "#ff6767",
    shadowSm: "0 1px 0 rgba(162,119,255,.14)", shadowMd: "0 2px 16px rgba(162,119,255,.18)",
  }),
  dark("nineteen-eighty-four", "1984 Dark", {
    bg: "#0d0f24", bgElev: "#161834", bgHover: "#222545", bgCanvas: "#080a1b", bgSelected: "#7b5cff1f", bgFrosted: "rgba(22,24,52,.88)",
    text: "#dcdfff", textDim: "#7a82b8", textBright: "#ffffff",
    border: "#222545", borderStrong: "#34386a", borderSubtle: "#161834",
    accent: "#7b5cff", accentText: "#ffffff", accentSoft: "#7b5cff33",
    danger: "#ff4f87", ok: "#5cffd0", warning: "#ffe16e",
    termBg: "#0d0f24", termFg: "#dcdfff",
    ansiGreen: "#5cffd0", ansiYellow: "#ffe16e", ansiBlue: "#5c8cff",
    ansiMagenta: "#c47bff", ansiCyan: "#5ce0ff", ansiRed: "#ff4f87",
    shadowSm: "0 1px 0 rgba(123,92,255,.14)", shadowMd: "0 2px 18px rgba(123,92,255,.2)",
  }),
  light("peach-fresh", "Peach Fresh", {
    bg: "#fce5d2", bgElev: "#f7d6bd", bgHover: "#f0c4a5", bgCanvas: "#ffe9d8", bgSelected: "#c64d2622", bgFrosted: "rgba(252,229,210,.88)",
    text: "#5a3a2b", textDim: "#9a7762", textBright: "#3a2418",
    border: "#f0c4a5", borderStrong: "#c89a7f", borderSubtle: "#f7d6bd",
    accent: "#c64d26", accentText: "#fce5d2", accentSoft: "#c64d2622",
    danger: "#c0392b", ok: "#7a8a26", warning: "#d68a25",
    termBg: "#fce5d2", termFg: "#5a3a2b",
    ansiGreen: "#7a8a26", ansiYellow: "#d68a25", ansiBlue: "#2670a3",
    ansiMagenta: "#a04079", ansiCyan: "#2f8a7d", ansiRed: "#c0392b",
    shadowSm: "0 1px 0 rgba(0,0,0,.05)", shadowMd: "0 2px 14px rgba(0,0,0,.08)",
  }),
];

export const DEFAULT_THEME_ID = "tersh-dark";
export const findTheme = (id: string): Theme =>
  THEMES.find(t => t.id === normalizeThemeId(id)) ?? THEMES[0]!;
