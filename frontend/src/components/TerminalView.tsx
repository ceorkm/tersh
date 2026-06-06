import { useEffect, useRef, useState } from "react";
import { Channel } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import { Paperclip, X, AlertCircle, Check, WandSparkles, RefreshCw, Loader2 } from "lucide-react";
import "@xterm/xterm/css/xterm.css";
import { api, base64ToBytes, pathLooksSensitive } from "../lib/api";
import { recordCommand } from "../lib/commandHistory";
import { getShellCwd, setShellCwd } from "../lib/shellCwd";
import { brainScopeKey, buildPromptEnhanceRequest } from "../lib/promptEnhancer";

interface TransferCard {
  id: string;
  name: string;
  isDir?: boolean;
  bytesDone: number;
  total: number;
  done: boolean;
  failed?: string;
}

interface PromptPreview {
  original: string;   // the raw prompt, kept so Regenerate can re-run it
  enhanced: string;
}

function basenameOf(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

function redactLocalPaths(message: string): string {
  return message
    .replace(/\/Users\/[^/\s"'`]+(?:\/[^\s"'`]*)?/g, "[local path]")
    .replace(/\/private\/var\/folders\/[^\s"'`]*/g, "[local temp path]")
    .replace(/\/var\/folders\/[^\s"'`]*/g, "[local temp path]");
}

function shellQuotePath(path: string): string {
  return "'" + path.replace(/'/g, "'\\''") + "'";
}

function isPasteableImagePath(path: string): boolean {
  return /\.(png|jpe?g|gif|tiff?)$/i.test(path);
}


const DEVICE_ATTRIBUTE_RESPONSE = /^(?:(?:\x1b\[\??[0-9;]*c)|(?:\x1b\[>[0-9;]*c))+$/;
const DEVICE_ATTRIBUTE_ESCAPE_PREFIX = /^\x1b\[(?:\??[0-9;]*|>[0-9;]*)$/;
const TERMINAL_EMOJI_GLYPH_FACES = '"Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"';
const SENSITIVE_PROMPT = /(?:password|passwd|passphrase|pin|otp|mfa|2fa|two[- ]factor|verification code|auth(?:entication)? code|token|secret|credential|api[-_ ]?key|private key|unlock|decrypt|keychain|login|username)[^\r\n]{0,120}[:?]\s*$/i;
const NORMAL_LOCAL_SHELL_PROMPT = /(?:^|\n)[^\n\r]{0,160}(?:[$%#❯➜λ])\s*$/;
const AGENT_TUI_MARKER = /\bClaude Code\b|\bCodex\b|\bbypass permissions on\b|\besc to interrupt\b|\bthinking with\b|\bBrewed for\b|\bSymbioting\b/i;
const NORMAL_SHELL_OUTPUT_MARKER = /\bWelcome to\b|\bLast login:\b|\bSystem information as of\b|(?:^|\n)[^\n\r]{0,160}(?:[$%#❯➜λ])\s*$/i;
// Bracketed paste: terminals wrap pasted text in these so the shell knows it's
// a paste (not typed). We capture the content between them as user input.
const BRACKETED_PASTE_START = "[200~";
const BRACKETED_PASTE_END = "[201~";
const TERMINAL_LINE_HEIGHT = 1.0;
const ACTIVE_AGENT_ROW_REFRESH_INTERVAL_MS = 700;
const ACTIVE_AGENT_FULL_HEAL_INTERVAL_MS = 1500;
const AGENT_TRAILING_RENDER_HEAL_DELAY_MS = 700;

/// Best-effort: read the user's current input from xterm's own buffer (the
/// logical line at the cursor, with the shell prompt stripped). Used as a
/// FALLBACK for the prompt enhancer when keystroke tracking missed the input
/// (↑-recall, odd pastes) — it sees whatever is actually on screen.
function readCurrentTerminalInput(term: Terminal): string {
  const buf = term.buffer.active;
  const cursorRow = buf.baseY + buf.cursorY;
  let start = cursorRow;
  while (start > 0) {
    const line = buf.getLine(start);
    if (line && line.isWrapped) start--;
    else break;
  }
  let text = "";
  for (let y = start; y <= cursorRow; y++) {
    const line = buf.getLine(y);
    if (line) text += line.translateToString(false);
  }
  text = text.replace(/\s+$/, "");
  // Strip the shell prompt: take everything after the FIRST prompt marker +
  // space (prompts sit at the line start). Best-effort; imperfect by nature.
  const m = text.match(/^.*?[$#%>❯➜λ]\s+/);
  return (m ? text.slice(m[0].length) : text).trim();
}

function xtermFontStack(fontStack: string | undefined): string {
  const base = (fontStack?.trim() || "ui-monospace, monospace").replace(/;$/, "");
  return /Apple Color Emoji|Segoe UI Emoji|Noto Color Emoji/i.test(base)
    ? base
    : `${base}, ${TERMINAL_EMOJI_GLYPH_FACES}`;
}

function stripTerminalDeviceAttributeResponses(data: string, pending: string): { text: string; pending: string } {
  const combined = pending + data;
  if (DEVICE_ATTRIBUTE_RESPONSE.test(combined)) {
    return { text: "", pending: "" };
  }
  if (DEVICE_ATTRIBUTE_ESCAPE_PREFIX.test(combined)) {
    return { text: "", pending: combined };
  }
  return { text: combined, pending: "" };
}

function stripTerminalControls(text: string): string {
  return text
    .replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "")
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
}

function dragPositionHitsElement(position: { x: number; y: number } | undefined, element: HTMLElement | null): boolean {
  if (!position || !element) return true;
  const rect = element.getBoundingClientRect();
  const hits = (x: number, y: number) => x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  if (hits(position.x, position.y)) return true;
  const scale = window.devicePixelRatio || 1;
  return scale !== 1 && hits(position.x / scale, position.y / scale);
}

import { findTheme } from "../themes";
import { FONT_OPTIONS, type Appearance } from "../lib/appearance";
import type { Tab, UploadResult } from "../types";

interface Props {
  tab: Tab;
  appearance: Appearance;
  onPendingChip: (chip: UploadResult | undefined) => void;
  initialOutput?: Uint8Array[];
  onOutputChunk?: (chunk: Uint8Array) => void;
  drawerOpen: boolean;
  allowUploads?: boolean;
  active?: boolean;
  onFocusRequest?: () => void;
}

export function TerminalView({
  tab,
  appearance,
  onPendingChip,
  initialOutput,
  onOutputChunk,
  drawerOpen: _drawerOpen,
  allowUploads = true,
  active = true,
  onFocusRequest,
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const resizeTimer = useRef<number | null>(null);
  const restoreTimer = useRef<number | null>(null);
  const lastFit = useRef<{ cols: number; rows: number } | null>(null);
  const pendingFit = useRef<{ cols: number; rows: number } | null>(null);
  const lastRenderHealAt = useRef(0);
  const lastActiveAgentRowRefreshAt = useRef(0);
  const lastActiveAgentFullHealAt = useRef(0);
  const activeAgentRowRefreshRaf = useRef<number | null>(null);
  const activeAgentFullHealRaf = useRef<number | null>(null);
  // Keep this legacy binding for React Fast Refresh: an old effect cleanup can
  // run after HMR with a closure that still references the pre-split RAF name.
  const activeAgentRenderHealRaf = activeAgentFullHealRaf;
  const activeAgentTrailingHealTimer = useRef<number | null>(null);
  const agentSawAlternateBuffer = useRef(false);
  const agentRenderDirty = useRef(false);
  const fitting = useRef(false);
  const unlisteners = useRef<Array<() => void>>([]);
  const dropUnlisten = useRef<(() => void) | null>(null);
  const outputChannel = useRef<Channel<ArrayBuffer | Uint8Array | number[]> | null>(null);
  const disposed = useRef(false);
  const pendingChipRef = useRef(onPendingChip);
  const outputChunkRef = useRef(onOutputChunk);
  const inputEncoder = useRef(new TextEncoder());
  const outputDecoder = useRef(new TextDecoder());
  const deviceAttributeInputBuffer = useRef("");
  const inputQueue = useRef<Uint8Array[]>([]);
  const inputQueuedBytes = useRef(0);
  const inputTimer = useRef<number | null>(null);
  const inputTrackQueue = useRef("");
  const inputTrackTimer = useRef<number | null>(null);
  const lastUserInputAt = useRef(0);
  const commandDraft = useRef("");
  const commandDraftUnsafe = useRef(false);
  // True while we're inside a bracketed paste (ESC[200~ … ESC[201~), possibly
  // spanning multiple onData chunks.
  const pasting = useRef(false);
  const recentOutput = useRef("");
  const agentImagePasteCapable = useRef(false);
  const suppressSensitiveInput = useRef(false);
  const lastInsertedUpload = useRef<{ text: string; at: number } | null>(null);
  const pendingChipDismissTimer = useRef<number | null>(null);
  const [dragging, setDragging] = useState(false);
  // Per-tab transfer dock: each in-flight upload renders as a card stacked
  // above the FAB with its own progress bar + cancel button. Uploads are
  // fire-and-forget — the terminal stays responsive while bytes flow.
  const [transfers, setTransfers] = useState<TransferCard[]>([]);
  const [promptPreview, setPromptPreview] = useState<PromptPreview | null>(null);
  const [promptEnhancing, setPromptEnhancing] = useState(false);
  const [promptEnhancerError, setPromptEnhancerError] = useState("");
  const transferUnlisten = useRef(new Map<string, () => void>());
  const transferDismissTimers = useRef(new Map<string, number>());
  const closedTransfers = useRef(new Set<string>());

  const sessionId = tab.state.kind === "connected" ? tab.state.sessionId : null;
  const sessionIdRef = useRef<string | null>(sessionId);

  useEffect(() => {
    agentImagePasteCapable.current = false;
    agentSawAlternateBuffer.current = false;
    agentRenderDirty.current = false;
    recentOutput.current = "";
  }, [tab.id, sessionId]);
  const activeRef = useRef(active);
  const isLocalTerminal = tab.kind === "local";

  useEffect(() => {
    activeRef.current = active;
  }, [active]);

  useEffect(() => {
    sessionIdRef.current = sessionId;
    deviceAttributeInputBuffer.current = "";
    lastFit.current = null;
    pendingFit.current = null;
  }, [sessionId]);

  useEffect(() => {
    disposed.current = false;
    return () => {
      disposed.current = true;
      if (pendingChipDismissTimer.current !== null) {
        window.clearTimeout(pendingChipDismissTimer.current);
        pendingChipDismissTimer.current = null;
      }
      if (inputTrackTimer.current !== null) {
        window.clearTimeout(inputTrackTimer.current);
        inputTrackTimer.current = null;
      }
      inputTrackQueue.current = "";
      if (restoreTimer.current !== null) {
        window.clearTimeout(restoreTimer.current);
        restoreTimer.current = null;
      }
      if (activeAgentRowRefreshRaf.current !== null) {
        cancelAnimationFrame(activeAgentRowRefreshRaf.current);
        activeAgentRowRefreshRaf.current = null;
      }
      if (activeAgentFullHealRaf.current !== null) {
        cancelAnimationFrame(activeAgentFullHealRaf.current);
        activeAgentFullHealRaf.current = null;
      }
      if (activeAgentRenderHealRaf.current !== null) {
        cancelAnimationFrame(activeAgentRenderHealRaf.current);
        activeAgentRenderHealRaf.current = null;
      }
      if (activeAgentTrailingHealTimer.current !== null) {
        window.clearTimeout(activeAgentTrailingHealTimer.current);
        activeAgentTrailingHealTimer.current = null;
      }
      for (const timer of transferDismissTimers.current.values()) {
        window.clearTimeout(timer);
      }
      transferDismissTimers.current.clear();
      for (const off of transferUnlisten.current.values()) {
        off();
      }
      transferUnlisten.current.clear();
      closedTransfers.current.clear();
    };
  }, []);

  const forceRendererClear = (term: Terminal) => {
    const core = (term as unknown as {
      _core?: { _renderService?: { clear?: () => void } };
    })._core;
    core?._renderService?.clear?.();
  };

  const forceRendererRows = (term: Terminal, start: number, end: number) => {
    const core = (term as unknown as {
      _core?: {
        _renderService?: {
          _renderer?: { value?: { renderRows?: (start: number, end: number) => void } };
        };
      };
    })._core;
    const renderRows = core?._renderService?._renderer?.value?.renderRows;
    if (renderRows) {
      renderRows.call(core?._renderService?._renderer?.value, start, end);
      return;
    }
    term.refresh(start, end);
  };

  const scheduleRendererHeal = (term: Terminal, minIntervalMs: number) => {
    const now = Date.now();
    if (now - lastActiveAgentFullHealAt.current < minIntervalMs) return;
    if (activeAgentFullHealRaf.current !== null) return;
    lastActiveAgentFullHealAt.current = now;

    activeAgentFullHealRaf.current = requestAnimationFrame(() => {
      const currentTerm = termRef.current;
      if (!currentTerm || !activeRef.current || currentTerm !== term) {
        activeAgentFullHealRaf.current = null;
        return;
      }
      forceRendererClear(currentTerm);
      activeAgentFullHealRaf.current = requestAnimationFrame(() => {
        activeAgentFullHealRaf.current = null;
        const repaintTerm = termRef.current;
        if (!repaintTerm || !activeRef.current || repaintTerm !== term) return;
        forceRendererRows(repaintTerm, 0, repaintTerm.rows - 1);
        repaintTerm.refresh(0, repaintTerm.rows - 1);
      });
    });
  };

  const scheduleAgentRowRefresh = (term: Terminal, minIntervalMs: number) => {
    const now = Date.now();
    if (now - lastActiveAgentRowRefreshAt.current < minIntervalMs) return;
    if (activeAgentRowRefreshRaf.current !== null) return;
    lastActiveAgentRowRefreshAt.current = now;

    activeAgentRowRefreshRaf.current = requestAnimationFrame(() => {
      activeAgentRowRefreshRaf.current = null;
      const currentTerm = termRef.current;
      if (!currentTerm || !activeRef.current || currentTerm !== term) return;
      if (!agentImagePasteCapable.current || currentTerm.buffer.active.type !== "alternate") return;
      forceRendererRows(currentTerm, 0, currentTerm.rows - 1);
    });
  };

  const scheduleTrailingAgentRenderHeal = () => {
    const term = termRef.current;
    if (!term || !activeRef.current || !agentImagePasteCapable.current) return;
    if (term.buffer.active.type !== "alternate") return;
    if (activeAgentTrailingHealTimer.current !== null) {
      window.clearTimeout(activeAgentTrailingHealTimer.current);
    }
    activeAgentTrailingHealTimer.current = window.setTimeout(() => {
      activeAgentTrailingHealTimer.current = null;
      const currentTerm = termRef.current;
      if (!currentTerm || !activeRef.current || currentTerm !== term) return;
      if (!agentImagePasteCapable.current || currentTerm.buffer.active.type !== "alternate") return;
      scheduleRendererHeal(currentTerm, ACTIVE_AGENT_FULL_HEAL_INTERVAL_MS);
    }, AGENT_TRAILING_RENDER_HEAL_DELAY_MS);
  };

  const healActiveAgentRenderIfNeeded = () => {
    const term = termRef.current;
    if (!term || !activeRef.current || !agentImagePasteCapable.current) return;
    if (term.buffer.active.type !== "alternate") {
      if (
        (agentSawAlternateBuffer.current || agentRenderDirty.current) &&
        NORMAL_SHELL_OUTPUT_MARKER.test(recentOutput.current)
      ) {
        agentSawAlternateBuffer.current = false;
        agentRenderDirty.current = false;
        scheduleRendererHeal(term, 0);
      }
      return;
    }
    if (term.rows <= 0) return;
    agentSawAlternateBuffer.current = true;
    scheduleAgentRowRefresh(term, ACTIVE_AGENT_ROW_REFRESH_INTERVAL_MS);
    scheduleTrailingAgentRenderHeal();
  };

  const shouldSendResize = (
    size: { cols: number; rows: number },
    forcePtyResize: boolean,
  ) => {
    const before = lastFit.current;
    const pending = pendingFit.current;
    if (pending && pending.cols === size.cols && pending.rows === size.rows) return false;
    if (forcePtyResize) return true;
    return !before || before.cols !== size.cols || before.rows !== size.rows;
  };

  const fitAndResize = (
    sid: string | null = sessionIdRef.current,
    refresh = false,
    opts: { forcePtyResize?: boolean; forceRenderClear?: boolean } = {},
  ) => {
    const term = termRef.current;
    const fit = fitRef.current;
    const container = containerRef.current;
    if (!term || !fit || !container || fitting.current) return;
    if (container.clientWidth < 20 || container.clientHeight < 20) return;
    fitting.current = true;
    try {
      fit.fit();
      const after = { cols: term.cols, rows: term.rows };
      if (
        sid &&
        after.cols > 0 &&
        after.rows > 0 &&
        shouldSendResize(after, Boolean(opts.forcePtyResize))
      ) {
        pendingFit.current = after;
        api.resize(sid, after.cols, after.rows)
          .then(() => {
            const pending = pendingFit.current;
            if (pending && pending.cols === after.cols && pending.rows === after.rows) {
              lastFit.current = after;
              pendingFit.current = null;
            }
          })
          .catch(() => {
            const pending = pendingFit.current;
            if (pending && pending.cols === after.cols && pending.rows === after.rows) {
              pendingFit.current = null;
            }
          });
      }
      if (opts.forceRenderClear) {
        const now = Date.now();
        if (now - lastRenderHealAt.current > 750) {
          lastRenderHealAt.current = now;
          forceRendererClear(term);
        }
      }
      if (refresh && term.rows > 0) {
        term.refresh(0, term.rows - 1);
        if (activeRef.current) term.scrollToBottom();
      }
    } catch {
    } finally {
      fitting.current = false;
    }
  };

  const fitAndResizeWhenQuiet = (
    sid: string | null = sessionIdRef.current,
    refresh = false,
    opts: { forcePtyResize?: boolean; forceRenderClear?: boolean } = {},
  ) => {
    const elapsed = Date.now() - lastUserInputAt.current;
    if (elapsed < 250) {
      if (resizeTimer.current !== null) window.clearTimeout(resizeTimer.current);
      resizeTimer.current = window.setTimeout(() => {
        resizeTimer.current = null;
        fitAndResize(sid, refresh, opts);
      }, 250 - elapsed);
      return;
    }
    fitAndResize(sid, refresh, opts);
  };

  const recoverAfterWindowGeometryChange = () => {
    if (!activeRef.current) return;
    if (restoreTimer.current !== null) {
      window.clearTimeout(restoreTimer.current);
      restoreTimer.current = null;
    }
    restoreTimer.current = window.setTimeout(() => {
      restoreTimer.current = null;
      requestAnimationFrame(() => fitAndResizeWhenQuiet(sessionIdRef.current, true, {
        forcePtyResize: true,
        forceRenderClear: true,
      }));
    }, 120);
  };

  // Focus THIS terminal. No cross-tile coordination — xterm handles focus
  // correctly on its own (clicking into one textarea naturally blurs any
  // other). The previous "activeTerminalOwner" module-global was blurring
  // the terminal a tick after it focused, which made `term.onData` never
  // fire and the user couldn't type. See diag log under ~/.tersh/diag.log
  // (zero term.onData lines despite the IO effect wiring correctly).
  const activateThisTerminal = () => {
    if (!activeRef.current) return;
    const term = termRef.current;
    if (!term) return;
    term.focus();
    term.textarea?.focus({ preventScroll: true });
    onFocusRequest?.();
  };

  useEffect(() => {
    pendingChipRef.current = onPendingChip;
  }, [onPendingChip]);

  useEffect(() => {
    outputChunkRef.current = onOutputChunk;
  }, [onOutputChunk]);

  // mount xterm once on first render with current theme
  useEffect(() => {
    if (!containerRef.current) return;
    const theme = findTheme(appearance.themeId);

    const term = new Terminal({
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily: xtermFontStack(getComputedStyle(document.documentElement).getPropertyValue("--term-font")),
      fontSize: appearance.fontSize,
      theme: theme.xterm,
      scrollback: 10000,
      allowProposedApi: true,
      // Do not let xterm inherit the surrounding app/chrome background. If a
      // light terminal theme supplies dark foreground text while the container
      // stays navy, typed text becomes unreadable. xterm must paint its own
      // matching foreground/background pair.
      allowTransparency: false,
      convertEol: false,
      windowsMode: false,
      macOptionIsMeta: false,
      macOptionClickForcesSelection: false,
      rightClickSelectsWord: false,
      fastScrollModifier: "alt",
      fastScrollSensitivity: 5,
      // xterm auto-brightens text whose contrast against the bg falls below
      // this ratio. Programs (Claude Code, agents, prompts) frequently use
      // ANSI dim/bright-black for descriptions — at the default of 1 they
      // render as illegible grey on dark themes. 4.5 = WCAG AA body-text
      // floor; xterm only adjusts when the requested colour is dimmer.
      minimumContrastRatio: 4.5,
      letterSpacing: 0,
      fontWeight: 500,
      fontWeightBold: 700,
      lineHeight: TERMINAL_LINE_HEIGHT,
    });
    const unicodeGraphemes = new UnicodeGraphemesAddon();
    const fit = new FitAddon();
    term.loadAddon(unicodeGraphemes);
    term.loadAddon(fit);
    term.open(containerRef.current);
    // Keep xterm on the grapheme-aware Unicode 15 provider. The addon sets
    // this during activate(), but keep it explicit here so future refactors
    // don't accidentally drop back to plain wcwidth tables.
    term.unicode.activeVersion = "15-graphemes";
    // Block OSC 52 from remote: a compromised server could otherwise rewrite
    // the local clipboard via terminal output. Returning true tells xterm.js
    // the sequence was handled; we drop it. MUST be after open() — the parser
    // is wired during open and earlier registration silently misses the handler.
    term.parser.registerOscHandler(52, () => true);
    // OSC 7: shells report their working directory as `file://host/path`.
    // Capture it (best-effort — only emitted if the shell is configured to) so
    // Browse and the upload suggestion can anchor to the live cwd. We consume
    // the sequence (return true); xterm has no built-in OSC 7 behaviour to run.
    term.parser.registerOscHandler(7, (data: string) => {
      const sid = sessionIdRef.current;
      if (sid) setShellCwd(sid, data);
      return true;
    });
    term.attachCustomKeyEventHandler((event) => {
      if (
        event.type === "keydown" &&
        event.shiftKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.altKey &&
        event.key === "Enter"
      ) {
        event.preventDefault();
        term.scrollToBottom();
        term.focus();
        const sid = sessionIdRef.current;
        if (sid) {
          // Shift+Enter must be distinguishable from Enter for agent prompts
          // like Claude Code. CSI u encodes Enter(13) + Shift(2).
          queueInput(sid, "\x1b[13;2u");
        }
        return false;
      }
      if (
        event.type === "keydown" &&
        event.ctrlKey &&
        !event.metaKey &&
        !event.altKey &&
        event.key.toLowerCase() === "p"
      ) {
        event.preventDefault();
        // This key handler is installed once at mount (dep [tab.id]); calling the
        // closed-over requestPromptEnhance would use the MOUNT-render session
        // (null while connecting) and a stale scope. Dispatch the request event
        // instead — its listener re-registers every render, so it runs with the
        // live session + scope (same path the drawer Ctrl+P button uses).
        window.dispatchEvent(new CustomEvent("tersh:prompt-enhance-request", {
          detail: { sessionId: sessionIdRef.current },
        }));
        return false;
      }
      if (
        event.type === "keydown" &&
        event.ctrlKey &&
        !event.metaKey &&
        !event.altKey &&
        event.key.toLowerCase() === "c"
      ) {
        term.scrollToBottom();
        term.focus();
      }
      return true;
    });

    termRef.current = term;
    fitRef.current = fit;
    const writeParsedDisposable = term.onWriteParsed(() => {
      healActiveAgentRenderIfNeeded();
    });
    if (activeRef.current) activateThisTerminal();
    const focusHandler = () => {
      if (activeRef.current) onFocusRequest?.();
    };
    const container = containerRef.current;
    container.addEventListener("focusin", focusHandler);

    // xterm's renderer initializes async after open(). Calling fit() in the
    // same tick throws "this._renderer.value.dimensions is undefined" in
    // WebKit. Defer to the next frame; if dimensions still look wrong, retry
    // once more.
    let raf1 = 0;
    let raf2 = 0;
    raf1 = requestAnimationFrame(() => {
      if (initialOutput && initialOutput.length > 0) {
        for (const chunk of initialOutput) term.write(chunk);
      }
      if (activeRef.current) fitAndResize(sessionIdRef.current, true);
      if (term.cols < 10 || term.rows < 3) {
        raf2 = requestAnimationFrame(() => {
          if (activeRef.current) fitAndResize(sessionIdRef.current, true);
        });
      }
    });

    const onResize = () => {
      if (resizeTimer.current !== null) window.clearTimeout(resizeTimer.current);
      resizeTimer.current = window.setTimeout(() => {
        resizeTimer.current = null;
        if (activeRef.current) fitAndResizeWhenQuiet(sessionIdRef.current);
      }, 50);
    };
    const ro = new ResizeObserver(onResize);
    ro.observe(containerRef.current);
    const windowTarget = getCurrentWindow();
    const windowUnlisteners: UnlistenFn[] = [];
    let cancelledWindowListeners = false;
    const trackWindowUnlisten = (promise: Promise<UnlistenFn>) => {
      promise.then(unlisten => {
        if (cancelledWindowListeners) unlisten();
        else windowUnlisteners.push(unlisten);
      }).catch(() => {});
    };
    const onWindowFocus = () => recoverAfterWindowGeometryChange();
    const onVisibilityChange = () => {
      if (!document.hidden) recoverAfterWindowGeometryChange();
    };
    window.addEventListener("focus", onWindowFocus);
    document.addEventListener("visibilitychange", onVisibilityChange);
    trackWindowUnlisten(windowTarget.onResized(() => recoverAfterWindowGeometryChange()));
    trackWindowUnlisten(windowTarget.onScaleChanged(() => recoverAfterWindowGeometryChange()));
    trackWindowUnlisten(windowTarget.onFocusChanged(({ payload }) => {
      if (payload) recoverAfterWindowGeometryChange();
    }));

    return () => {
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
      cancelledWindowListeners = true;
      for (const unlisten of windowUnlisteners) unlisten();
      window.removeEventListener("focus", onWindowFocus);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      ro.disconnect();
      if (resizeTimer.current !== null) {
        window.clearTimeout(resizeTimer.current);
        resizeTimer.current = null;
      }
      if (restoreTimer.current !== null) {
        window.clearTimeout(restoreTimer.current);
        restoreTimer.current = null;
      }
      if (activeAgentRowRefreshRaf.current !== null) {
        cancelAnimationFrame(activeAgentRowRefreshRaf.current);
        activeAgentRowRefreshRaf.current = null;
      }
      if (activeAgentFullHealRaf.current !== null) {
        cancelAnimationFrame(activeAgentFullHealRaf.current);
        activeAgentFullHealRaf.current = null;
      }
      if (activeAgentRenderHealRaf.current !== null) {
        cancelAnimationFrame(activeAgentRenderHealRaf.current);
        activeAgentRenderHealRaf.current = null;
      }
      if (activeAgentTrailingHealTimer.current !== null) {
        window.clearTimeout(activeAgentTrailingHealTimer.current);
        activeAgentTrailingHealTimer.current = null;
      }
      writeParsedDisposable.dispose();
      term.dispose();
      container.removeEventListener("focusin", focusHandler);
      termRef.current = null;
    };
  }, [tab.id]);

  useEffect(() => {
    if (!active || !sessionId) return;
    let raf1 = 0;
    let raf2 = 0;
    raf1 = requestAnimationFrame(() => {
      fitAndResizeWhenQuiet(sessionId, true);
      raf2 = requestAnimationFrame(() => fitAndResizeWhenQuiet(sessionId, true));
    });
    return () => {
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
    };
  }, [active, sessionId]);

  // Direct write — xterm.js already has its own internal write queue with rAF
  // scheduling and handles backpressure correctly. Layering our own queue on
  // top throttled throughput to one chunk per frame (~60 chunks/sec), which
  // was the freeze.
  const enqueueWrite = (chunk: string | Uint8Array) => {
    const term = termRef.current;
    if (!term) return;
    noteTerminalOutput(chunk);
    term.write(chunk);
  };

  const noteTerminalOutput = (chunk: string | Uint8Array) => {
    // Only inspect the tail. This runs on EVERY output packet; agent-name
    // detection and the sensitive-prompt guard only ever look at recent
    // output (recentOutput is capped at 500 chars, the prompt check at 160),
    // so decoding + regex-stripping a multi-megabyte burst in full is pure
    // waste that stalls rendering on high-churn output (agent TUIs, holding
    // an arrow key through long history). Cap the work to a fixed tail.
    const TAIL = 2048;
    const text = typeof chunk === "string"
      ? (chunk.length > TAIL ? chunk.slice(-TAIL) : chunk)
      : outputDecoder.current.decode(chunk.length > TAIL ? chunk.subarray(chunk.length - TAIL) : chunk);
    if (!text) return;
    const normalized = stripTerminalControls(text);
    if (AGENT_TUI_MARKER.test(normalized)) {
      agentImagePasteCapable.current = true;
      agentRenderDirty.current = true;
    }
    recentOutput.current = (recentOutput.current + normalized).slice(-500);
    const tail = recentOutput.current.slice(-160);
    if (SENSITIVE_PROMPT.test(tail)) {
      suppressSensitiveInput.current = true;
      commandDraft.current = "";
      commandDraftUnsafe.current = true;
    }
  };

  const asBytes = (message: ArrayBuffer | Uint8Array | number[]) => {
    if (message instanceof Uint8Array) return message;
    if (message instanceof ArrayBuffer) return new Uint8Array(message);
    return new Uint8Array(message);
  };

  const flushInput = (sid: string) => {
    if (inputTimer.current !== null) {
      window.clearTimeout(inputTimer.current);
      inputTimer.current = null;
    }
    const queue = inputQueue.current;
    if (queue.length === 0) return;
    const total = inputQueuedBytes.current;
    inputQueue.current = [];
    inputQueuedBytes.current = 0;

    const payload = new Uint8Array(total);
    let offset = 0;
    for (const chunk of queue) {
      payload.set(chunk, offset);
      offset += chunk.byteLength;
    }

    // Fire-and-forget: the backend owns ordered writes through the SSH input
    // queue. Awaiting or chaining here lets one slow IPC make the terminal feel
    // frozen. Do NOT add a diag IPC here — a second Tauri invoke on every
    // keystroke batch is exactly what made typing sticky before (see kratos
    // mem 2026-05-21: per-keystroke diag logging was THE cause). One invoke
    // per flush, nothing else in the hot path.
    api.sendInputRaw(sid, payload).catch(() => {});
  };

  const shouldFlushInputImmediately = (data: string) => {
    for (const ch of data) {
      if (ch === "\u001b" || ch === "\u007f" || ch < " ") return true;
    }
    return false;
  };

  const queueInput = (sid: string, data: string) => {
    lastUserInputAt.current = Date.now();
    const bytes = inputEncoder.current.encode(data);
    inputQueue.current.push(bytes);
    inputQueuedBytes.current += bytes.byteLength;

    if (shouldFlushInputImmediately(data) || inputQueuedBytes.current >= 2048) {
      flushInput(sid);
      return;
    }
    if (inputTimer.current === null) {
      // Micro-batch printable text into one Tauri invoke. Control keys
      // (arrows, backspace, Enter, Ctrl) flush above because cursor movement
      // should feel instant; printable bursts benefit from less IPC churn.
      inputTimer.current = window.setTimeout(() => flushInput(sid), 2);
    }
  };

  // Core enhance call — runs the given prompt text and shows the result.
  // Shared by the initial Ctrl+P and the Regenerate button.
  const runEnhance = async (promptText: string) => {
    setPromptEnhancing(true);
    setPromptEnhancerError("");
    setPromptPreview(null);
    try {
      // Per-VPS: send the project selected for THIS terminal's connection.
      const scopeKey = brainScopeKey(
        tab.kind === "local" ? "local" : "remote",
        tab.kind === "local" ? null : sessionId,
        tab.kind === "local" ? (tab.localStartCwd ?? tab.localCwd ?? null) : null,
      );
      const req = buildPromptEnhanceRequest(promptText, sessionId, scopeKey);
      const response = await api.promptEnhance(req);
      setPromptPreview({ original: promptText, enhanced: response.enhanced_prompt });
    } catch (err) {
      setPromptEnhancerError(err instanceof Error ? err.message : String(err));
    } finally {
      setPromptEnhancing(false);
    }
  };

  const requestPromptEnhance = async () => {
    if (promptEnhancing) return;
    // Local terminals keep a cheap command draft for history. SSH terminals
    // avoid per-keystroke tracking on the hot path, so read their live line
    // directly when the user invokes Enhance.
    let prompt = isLocalTerminal ? commandDraft.current.trim() : "";
    if (termRef.current && (!prompt || !isLocalTerminal)) {
      prompt = readCurrentTerminalInput(termRef.current).trim();
    }
    if (!prompt) {
      setPromptEnhancerError("Type a prompt in the terminal first.");
      setPromptPreview(null);
      return;
    }
    await runEnhance(prompt);
  };

  // Re-run the enhancement on the SAME original prompt — for when you don't
  // like the result and want another take.
  const regenerateEnhance = () => {
    if (promptEnhancing || !promptPreview) return;
    void runEnhance(promptPreview.original);
  };

  // Copy the enhanced prompt to the local clipboard (user-initiated).
  const copyEnhancedPrompt = () => {
    if (promptPreview?.enhanced) {
      void navigator.clipboard?.writeText(promptPreview.enhanced).catch(() => {});
    }
  };

  // Pasted text (incl. voice-to-text) is user input. Capture it into the draft,
  // flattening newlines/tabs to spaces so a multi-line paste becomes one prompt.
  const appendPastedInput = (text: string) => {
    const cleaned = text.replace(/[\r\n\t]+/g, " ");
    // The draft feeds two things: the prompt captured for Enhance (Ctrl+P) and
    // the local command-history record. (There is no terminal-clear step anymore
    // — the enhancer just shows the result + Copy.) We append only printable
    // chars (ASCII space..~, or >= U+00A0) so control bytes never pollute the
    // captured prompt, and mark the draft unsafe on any \r\n\t / control so the
    // history gate skips an ambiguous line. Sticky across a multi-chunk paste.
    let safe = !/[\r\n\t]/.test(text);
    for (const ch of cleaned) {
      if ((ch >= " " && ch <= "~") || ch >= "\u00a0") {
        commandDraft.current += ch;
      } else {
        safe = false; // C0 control, DEL (0x7f), or C1 control (0x80–0x9f)
      }
    }
    commandDraftUnsafe.current = commandDraftUnsafe.current || !safe;
  };

  // Entry point for all terminal input. Splits the stream into typed segments
  // (handled by trackTypedCommand) and bracketed-paste segments (ESC[200~ …
  // ESC[201~, possibly spanning chunks). The old code fed raw data straight to
  // trackTypedCommand, which bailed on the paste marker's leading ESC — so
  // pasted prompts were never captured. This fixes the voice-to-text flow.
  const trackInput = (data: string) => {
    let rest = data;
    while (rest.length > 0) {
      if (pasting.current) {
        const end = rest.indexOf(BRACKETED_PASTE_END);
        if (end === -1) {
          appendPastedInput(rest);
          return;
        }
        appendPastedInput(rest.slice(0, end));
        pasting.current = false;
        rest = rest.slice(end + BRACKETED_PASTE_END.length);
        continue;
      }
      const start = rest.indexOf(BRACKETED_PASTE_START);
      if (start === -1) {
        trackTypedCommand(rest);
        return;
      }
      if (start > 0) trackTypedCommand(rest.slice(0, start));
      pasting.current = true;
      rest = rest.slice(start + BRACKETED_PASTE_START.length);
    }
  };

  const trackTypedCommand = (data: string) => {
    if (data.startsWith("\u001b")) {
      commandDraftUnsafe.current = true;
      return;
    }
    for (const ch of data) {
      if (suppressSensitiveInput.current) {
        if (ch === "\r" || ch === "\n" || ch === "\u0003") {
          suppressSensitiveInput.current = false;
          commandDraft.current = "";
          commandDraftUnsafe.current = false;
        }
        continue;
      }
      if (ch === "\r" || ch === "\n") {
        const tail = recentOutput.current.slice(-240);
        if (
          tab.kind === "local" &&
          !commandDraftUnsafe.current &&
          NORMAL_LOCAL_SHELL_PROMPT.test(tail) &&
          !SENSITIVE_PROMPT.test(tail)
        ) {
          recordCommand(commandDraft.current, tab.host);
        }
        commandDraft.current = "";
        commandDraftUnsafe.current = false;
        continue;
      }
      if (ch === "\u0003") {
        commandDraft.current = "";
        commandDraftUnsafe.current = false;
        continue;
      }
      if (ch === "\u007f" || ch === "\b") {
        // Delete one whole code point, not one UTF-16 unit. Dropping only the
        // low surrogate of an astral char (emoji) would leave a lone surrogate
        // in the draft and corrupt the prompt captured for Enhance. Kept O(1)
        // (no full-string spread) — this is the per-keystroke hot path.
        const d = commandDraft.current;
        const n = d.length;
        const isPair =
          n >= 2 &&
          d.charCodeAt(n - 1) >= 0xdc00 && d.charCodeAt(n - 1) <= 0xdfff &&
          d.charCodeAt(n - 2) >= 0xd800 && d.charCodeAt(n - 2) <= 0xdbff;
        commandDraft.current = d.slice(0, isPair ? -2 : -1);
        continue;
      }
      if (ch < " " || ch === "\u007f") {
        commandDraftUnsafe.current = true;
        continue;
      }
      if (ch >= " " && ch !== "\u007f") {
        commandDraft.current += ch;
      }
    }
  };

  const flushTrackedInput = () => {
    inputTrackTimer.current = null;
    const data = inputTrackQueue.current;
    inputTrackQueue.current = "";
    if (data) trackInput(data);
  };

  const queueTrackedInput = (data: string) => {
    inputTrackQueue.current += data;
    if (inputTrackTimer.current === null) {
      inputTrackTimer.current = window.setTimeout(flushTrackedInput, 50);
    }
  };

  const insertUploadedPath = (sid: string, text: string) => {
    const insertText = text.endsWith(" ") || text.endsWith("\n") || text.endsWith("\r")
      ? text
      : `${text} `;
    const now = Date.now();
    const last = lastInsertedUpload.current;
    // Tauri/webview drops can occasionally double-fire the same file event
    // during HMR or tab remounts. Never paste the exact same file reference
    // twice inside a short window.
    if (last && last.text === insertText && now - last.at < 2500) return false;
    lastInsertedUpload.current = { text: insertText, at: now };
    queueInput(sid, insertText);
    termRef.current?.scrollToBottom();
    return true;
  };

  const localAgentAcceptsImagePaste = () => {
    const output = recentOutput.current;
    return agentImagePasteCapable.current || AGENT_TUI_MARKER.test(output);
  };

  const insertLocalDroppedPaths = async (sid: string, paths: string[]) => {
    const imagePaths = paths.filter(isPasteableImagePath);
    const textPaths = paths.filter(path => !isPasteableImagePath(path));
    if (imagePaths.length > 0 && localAgentAcceptsImagePaste()) {
      for (const path of imagePaths) {
        try {
          await api.copyLocalImageToClipboard(path);
          queueInput(sid, "\u0016");
        } catch (err) {
          enqueueWrite(`\r\n\x1b[31m[image paste failed: ${err}]\x1b[0m\r\n`);
          queueInput(sid, shellQuotePath(path));
        }
      }
      if (textPaths.length > 0) {
        queueInput(sid, " " + textPaths.map(shellQuotePath).join(" "));
      }
      return;
    }
    const text = paths.map(shellQuotePath).join(" ");
    if (text) queueInput(sid, text);
  };

  // Apply this tab's `appearance` directly to xterm. Each TerminalView is
  // self-contained (Collaborator can render multiple tiles with different
  // themes); reading from `:root` would force every tile to inherit chrome.
  //
  // The dep array uses the theme OBJECT reference (not just themeId): when
  // themes/index.ts hot-reloads or we tweak a theme's foreground colour at
  // runtime, the object identity changes and this effect re-fires. That
  // makes live theme edits show up without a full app refresh.
  //
  // WKWebView quirk: setting term.options.theme mid-session can leave the
  // viewport blank until the next write — term.refresh(0, rows-1) forces a
  // repaint of existing scrollback in the new colours.
  const xtermTheme = findTheme(appearance.themeId).xterm;
  const fontStack = xtermFontStack(FONT_OPTIONS.find(f => f.id === appearance.fontId)?.stack);
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.theme = xtermTheme;
    let shouldRefit = false;
    if (fontStack && term.options.fontFamily !== fontStack) {
      term.options.fontFamily = fontStack;
      shouldRefit = true;
    }
    if (term.options.fontSize !== appearance.fontSize) {
      term.options.fontSize = appearance.fontSize;
      shouldRefit = true;
    }
    if (term.options.fontWeight !== 500) {
      term.options.fontWeight = 500;
    }
    if (term.options.fontWeightBold !== 700) {
      term.options.fontWeightBold = 700;
    }
    if (term.options.lineHeight !== TERMINAL_LINE_HEIGHT) {
      term.options.lineHeight = TERMINAL_LINE_HEIGHT;
      shouldRefit = true;
    }
    if (activeRef.current && shouldRefit) {
      fitAndResizeWhenQuiet(sessionId);
    }
    if (term.rows > 0) {
      term.refresh(0, term.rows - 1);
    }
  }, [xtermTheme, fontStack, appearance.fontSize]);

  // wire IO once we have a session
  useEffect(() => {
    if (!sessionId) return;
    const term = termRef.current;
    if (!term) return;

    const sub = term.onData(d => {
      const filtered = stripTerminalDeviceAttributeResponses(d, deviceAttributeInputBuffer.current);
      deviceAttributeInputBuffer.current = filtered.pending;
      if (!filtered.text) return;
      const data = filtered.text;
      const sid = sessionIdRef.current;
      if (sid) queueInput(sid, data);
      // SSH prompt enhancement reads the live terminal line on Ctrl+P, so do
      // not spend extra work tracking every remote keystroke. Keep tracking for
      // local terminals where it feeds local command history.
      if (isLocalTerminal) queueTrackedInput(data);
    });

    let unmounted = false;
    const channel = new Channel<ArrayBuffer | Uint8Array | number[]>((message) => {
      if (unmounted) {
        return;
      }
      const bytes = asBytes(message);
      enqueueWrite(bytes);
      queueMicrotask(() => {
        if (!unmounted) outputChunkRef.current?.(bytes);
      });
    });
    outputChannel.current = channel;
    api.bindTerminalOutput(sessionId, channel)
      .catch((err) => {
        api.diagLog(`FE bindTerminalOutput FAILED sid=${sessionId} err=${String(err).slice(0, 200)}`);
        if (outputChannel.current === channel) outputChannel.current = null;
      });

    api.onSessionOutput(
      sessionId,
      b64 => {
        if (unmounted) return;
        const bytes = base64ToBytes(b64);
        enqueueWrite(bytes);
        queueMicrotask(() => {
          if (!unmounted) outputChunkRef.current?.(bytes);
        });
      },
      b64 => {
        if (unmounted) return;
        const bytes = base64ToBytes(b64);
        enqueueWrite(bytes);
        queueMicrotask(() => {
          if (!unmounted) outputChunkRef.current?.(bytes);
        });
      },
      () => {
        if (unmounted) return;
        enqueueWrite("\r\n\x1b[33m[session closed]\x1b[0m\r\n");
        if (tab.kind === "local") {
          api.disconnect(sessionId).catch(() => {});
        }
      },
    ).then(uls => {
      if (unmounted || disposed.current) {
        for (const fn of uls) fn();
        return;
      }
      unlisteners.current = uls;
    });

    // Drag-drop via Tauri webview events (path-aware). SSH terminals upload
    // local files to the remote agent cwd; local terminals insert shell-safe
    // local paths directly into the prompt.
    (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const wv = getCurrentWebview();
        const unlisten = await wv.onDragDropEvent(async (e) => {
          if (unmounted) return;
          if (e.payload.type === "enter" || e.payload.type === "over") {
            setDragging(dragPositionHitsElement(e.payload.position, containerRef.current));
          }
          if (e.payload.type === "leave") setDragging(false);
          if (e.payload.type === "drop") {
            setDragging(false);
            if (!activeRef.current || !dragPositionHitsElement(e.payload.position, containerRef.current)) return;
            const paths = Array.from(new Set(e.payload.paths.filter(Boolean)));
            if (paths.length === 0) return;
            if (tab.kind === "local" || !allowUploads) {
              await insertLocalDroppedPaths(sessionId, paths);
              return;
            }
            const accepted: string[] = [];
            for (const path of paths) {
              if (pathLooksSensitive(path)) {
                enqueueWrite(`\r\n\x1b[31m[upload blocked: sensitive file]\x1b[0m\r\n`);
                continue;
              }
              accepted.push(path);
            }
            for (const path of accepted) {
              kickoffUpload(sessionId, path);
            }
          }
        });
        if (unmounted) {
          unlisten();
          return;
        }
        dropUnlisten.current = unlisten;
      } catch {
        // Tauri not running (dev mode in browser) — drag-drop unavailable
      }
    })();

    return () => {
      unmounted = true;
      // Clear any pending 4ms input batch timer — without this, a setTimeout
      // queued just before disconnect could fire AFTER cleanup and try to
      // send to the now-dead session.
      if (inputTimer.current !== null) {
        window.clearTimeout(inputTimer.current);
        inputTimer.current = null;
      }
      if (inputTrackTimer.current !== null) {
        window.clearTimeout(inputTrackTimer.current);
        inputTrackTimer.current = null;
        flushTrackedInput();
      }
      flushInput(sessionId);
      sub.dispose();
      if (outputChannel.current === channel) outputChannel.current = null;
      for (const fn of unlisteners.current) fn();
      unlisteners.current = [];
      if (dropUnlisten.current) { dropUnlisten.current(); dropUnlisten.current = null; }
    };
  }, [allowUploads, sessionId, tab.host.label, tab.kind]);

  const runUpload = (sid: string, localPath: string, transferId: string, isDir = false) => {
    // Listener first so we don't miss the first progress packet.
    api.onTransferProgress(transferId, p => {
      if (disposed.current) return;
      setTransfers(prev => prev.map(t =>
        t.id === transferId
          ? { ...t, bytesDone: p.bytes_done, total: p.total, done: p.done }
          : t,
      ));
    }).then(off => {
      if (disposed.current || closedTransfers.current.has(transferId)) {
        off();
        return;
      }
      transferUnlisten.current.set(transferId, off);
    });
    const uploadStartedAt = performance.now();
    api.diagLog(`upload start tid=${transferId} sid=${sid}`);
    // Anchor the upload target to the live shell cwd (OSC 7) when there's no
    // detected agent cwd — the backend uses it as the fallback drop dir.
    const preferredDir = getShellCwd(sid);
    const upload = isDir
      ? api.uploadFolderLocal(sid, localPath, tab.host.label, transferId, preferredDir)
      : api.uploadLocal(sid, localPath, tab.host.label, transferId, preferredDir);
    upload
      .then(res => {
        const elapsedMs = Math.round(performance.now() - uploadStartedAt);
        api.diagLog(`upload done tid=${transferId} bytes=${res.bytes_written} ms=${elapsedMs}`);
        if (disposed.current) return;
        // Insert the path through the SAME input queue user keystrokes use.
        // Without this, the path can race the user's next Enter through Tauri IPC.
        const insertStart = performance.now();
        const inserted = insertUploadedPath(sid, res.formatted_for_agent);
        api.diagLog(`upload insertPath ms=${Math.round(performance.now() - insertStart)} inserted=${inserted}`);
        if (!inserted) {
          scheduleTransferDismiss(transferId, 1200);
          return;
        }
        // Yield to the event loop before the cascade of parent setState +
        // dismiss timers — gives xterm a microtask window to flush any
        // pending output and re-paint, so the terminal feels responsive
        // immediately after the path lands.
        queueMicrotask(() => {
          if (disposed.current) return;
          pendingChipRef.current(res);
          if (pendingChipDismissTimer.current !== null) {
            window.clearTimeout(pendingChipDismissTimer.current);
          }
          pendingChipDismissTimer.current = window.setTimeout(() => {
            if (!disposed.current) pendingChipRef.current(undefined);
            pendingChipDismissTimer.current = null;
          }, 4000);
          scheduleTransferDismiss(transferId, 2500);
        });
      })
      .catch(err => {
        if (disposed.current) return;
        const message = redactLocalPaths(String(err).replace(/^Error:\s*/, ""));
        setTransfers(prev => prev.map(t =>
          t.id === transferId
            ? { ...t, done: true, failed: message }
            : t,
        ));
        enqueueWrite(`\r\n\x1b[31m[upload failed: ${message}]\x1b[0m\r\n`);
      });
  };

  // Fire-and-forget single upload. Pre-allocates a transfer id, subscribes to
  // backend progress events BEFORE kicking off the upload (no race window),
  // and inserts the formatted remote path into the terminal on success. The
  // terminal stays responsive throughout — bytes flow in a background task
  // while the user keeps typing.
  const kickoffUpload = (sid: string, localPath: string, preallocatedTransferId?: string, isDir = false) => {
    if (disposed.current) return;
    const transferId = preallocatedTransferId ?? crypto.randomUUID();
    closedTransfers.current.delete(transferId);
    const name = basenameOf(localPath);
    setTransfers(prev => [
      ...prev,
      { id: transferId, name, isDir, bytesDone: 0, total: 0, done: false },
    ]);
    runUpload(sid, localPath, transferId, isDir);
  };

  const kickoffUploadBatch = (sid: string, items: Array<{ localPath: string; transferId: string; isDir?: boolean }>) => {
    if (disposed.current || items.length === 0) return;
    for (const item of items) {
      closedTransfers.current.delete(item.transferId);
    }
    setTransfers(prev => [
      ...prev,
      ...items.map(item => ({
        id: item.transferId,
        name: basenameOf(item.localPath),
        isDir: item.isDir,
        bytesDone: 0,
        total: 0,
        done: false,
      })),
    ]);
    for (const item of items) {
      runUpload(sid, item.localPath, item.transferId, Boolean(item.isDir));
    }
  };

  const scheduleTransferDismiss = (id: string, after: number) => {
    if (disposed.current) return;
    const prior = transferDismissTimers.current.get(id);
    if (prior !== undefined) window.clearTimeout(prior);
    const timer = window.setTimeout(() => {
      if (disposed.current) return;
      setTransfers(prev => prev.filter(t => t.id !== id));
      closedTransfers.current.add(id);
      const off = transferUnlisten.current.get(id);
      if (off) off();
      transferUnlisten.current.delete(id);
      transferDismissTimers.current.delete(id);
    }, after);
    transferDismissTimers.current.set(id, timer);
  };

  const dismissTransfer = (id: string) => {
    if (disposed.current) return;
    const prior = transferDismissTimers.current.get(id);
    if (prior !== undefined) window.clearTimeout(prior);
    transferDismissTimers.current.delete(id);
    closedTransfers.current.add(id);
    const off = transferUnlisten.current.get(id);
    if (off) off();
    transferUnlisten.current.delete(id);
    setTransfers(prev => prev.filter(t => t.id !== id));
  };

  const cancelTransfer = (id: string) => {
    if (disposed.current) return;
    api.sftpCancelTransfer(id).catch(() => {});
    // Mark the card as failed/cancelled immediately so the user sees the
    // intent applied without waiting for the next progress event.
    setTransfers(prev => prev.map(t =>
      t.id === id && !t.done
        ? { ...t, done: true, failed: "cancelled" }
        : t,
    ));
    scheduleTransferDismiss(id, 1500);
  };

  // One picker, both files and folders. Each picked item already carries
  // is_dir (resolved from real filesystem metadata in the backend), so mixed
  // selections route to the file vs. folder SFTP path correctly below.
  const handleUpload = async () => {
    if (!sessionId || !allowUploads) return;
    let picked: { local_path: string; transfer_id: string; is_dir: boolean }[] = [];
    try {
      picked = await api.pickUploadsAny();
    } catch (err) {
      enqueueWrite(`\r\n\x1b[31m[picker failed: ${redactLocalPaths(String(err))}]\x1b[0m\r\n`);
      return;
    }
    const accepted: Array<{ localPath: string; transferId: string; isDir?: boolean }> = [];
    for (const item of picked) {
      const looksSensitive = !item.is_dir && pathLooksSensitive(item.local_path);
      if (looksSensitive) {
        enqueueWrite(`\r\n\x1b[31m[upload blocked: sensitive file]\x1b[0m\r\n`);
        continue;
      }
      accepted.push({
        localPath: item.local_path,
        transferId: item.transfer_id,
        isDir: item.is_dir,
      });
    }
    kickoffUploadBatch(sessionId, accepted);
  };

  useEffect(() => {
    const onUploadRequest = (event: Event) => {
      const detail = (event as CustomEvent<{ sessionId?: string | null }>).detail;
      if (!sessionId || detail?.sessionId !== sessionId || !allowUploads) return;
      void handleUpload();
    };
    window.addEventListener("tersh:upload-request", onUploadRequest);
    return () => window.removeEventListener("tersh:upload-request", onUploadRequest);
  }, [allowUploads, sessionId]);

  useEffect(() => {
    const onPromptEnhanceRequest = (event: Event) => {
      const detail = (event as CustomEvent<{ sessionId?: string | null }>).detail;
      if (!sessionId || detail?.sessionId !== sessionId) return;
      void requestPromptEnhance();
    };
    window.addEventListener("tersh:prompt-enhance-request", onPromptEnhanceRequest);
    return () => window.removeEventListener("tersh:prompt-enhance-request", onPromptEnhanceRequest);
  }, [sessionId, promptEnhancing, tab.localCwd, tab.localStartCwd, tab.kind]);

  return (
    <div className="term-region">
      <div
        className={"term-mount" + (dragging ? " dragging" : "")}
        ref={containerRef}
        onPointerDownCapture={activateThisTerminal}
        onPointerDown={activateThisTerminal}
        onMouseDownCapture={activateThisTerminal}
        onWheel={activateThisTerminal}
        onTouchStart={activateThisTerminal}
      />

      {allowUploads && transfers.length > 0 && (
        <div className="term-upload-stack" role="status" aria-live="polite">
          {transfers.map(t => {
            const pct = t.total > 0
              ? Math.min(100, (t.bytesDone / t.total) * 100)
              : (t.done ? 100 : 0);
            const status: "running" | "done" | "failed" =
              t.failed ? "failed" : t.done ? "done" : "running";
            return (
              <div
                key={t.id}
                className={"term-upload-card " + status}
                title={t.failed ?? t.name}
              >
                <div className="term-upload-meta">
                  <span className="term-upload-icon">
                    {status === "failed"
                      ? <AlertCircle size={12} strokeWidth={2} />
                      : status === "done"
                        ? <Check size={12} strokeWidth={2} />
                        : <Paperclip size={12} strokeWidth={1.75} />}
                  </span>
                  <span className="term-upload-name">{t.name}</span>
                  <span className="term-upload-bytes">
                    {status === "failed"
                      ? t.failed
                      : status === "done"
                        ? null
                        : t.total > 0
                          ? `${Math.round(pct)}%`
                          : null}
                  </span>
                  <button
                    type="button"
                    className="term-upload-x"
                    aria-label={status === "running" ? "Cancel upload" : "Dismiss upload"}
                    onClick={(e) => {
                      e.stopPropagation();
                      if (status === "running") {
                        cancelTransfer(t.id);
                      } else {
                        dismissTransfer(t.id);
                      }
                    }}
                  >
                    <X size={11} />
                  </button>
                </div>
                <div
                  className={
                    "term-upload-bar" +
                    (status === "running" && t.total === 0 ? " indeterminate" : "")
                  }
                  aria-hidden
                  role="progressbar"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={t.total > 0 ? Math.round(pct) : undefined}
                >
                  <div className="term-upload-fill" style={{ width: `${pct}%` }} />
                </div>
              </div>
            );
          })}
        </div>
      )}
      {(promptEnhancing || promptEnhancerError || promptPreview) && (
        <div className="prompt-enhancer-popover" role="dialog" aria-label="Prompt enhancer">
          <div className="prompt-enhancer-popover-head">
            <span><WandSparkles size={13} strokeWidth={2} /> Prompt enhancer</span>
            <button
              type="button"
              aria-label="Close prompt enhancer"
              onClick={() => {
                setPromptPreview(null);
                setPromptEnhancerError("");
              }}
            >
              <X size={13} />
            </button>
          </div>
          {promptEnhancing ? (
            <div className="prompt-enhancer-state">
              <Loader2 size={13} strokeWidth={2.25} className="spin" />
              <span>Enhancing…</span>
            </div>
          ) : promptEnhancerError ? (
            <div className="prompt-enhancer-error">{promptEnhancerError}</div>
          ) : promptPreview ? (
            <>
              <textarea
                className="prompt-enhancer-output"
                value={promptPreview.enhanced}
                onChange={e => setPromptPreview({ ...promptPreview, enhanced: e.target.value })}
                spellCheck
              />
              <div className="prompt-enhancer-actions">
                <button type="button" className="primary" onClick={copyEnhancedPrompt}>Copy</button>
                <button type="button" className="ghost" onClick={regenerateEnhance} disabled={promptEnhancing}>
                  <RefreshCw size={12} strokeWidth={2} /> Regenerate
                </button>
              </div>
            </>
          ) : null}
        </div>
      )}
      {/* Drag overlay */}
      {dragging && (
        <div className="drop-overlay">
          <div className="drop-card">
            <Paperclip size={28} strokeWidth={1.5} />
            <div className="drop-title">{isLocalTerminal ? "Drop to insert path" : "Drop to attach"}</div>
            <div className="drop-sub">
              {isLocalTerminal
                ? "Local file path will be inserted into the prompt."
                : "Path will be inserted into the prompt for the remote agent."}
            </div>
          </div>
        </div>
      )}

      {/* Drawer toggle lives in the topbar now (same icon opens + closes) —
          one affordance, always visible, no orphan button in the terminal. */}
    </div>
  );
}
