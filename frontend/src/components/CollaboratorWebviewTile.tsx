import { useEffect, useMemo, useRef, useState } from "react";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import { Webview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emitTo, type UnlistenFn } from "@tauri-apps/api/event";
import { normalizeAppearance, type Appearance } from "../lib/appearance";
import { findTheme } from "../themes";
import type { Tab } from "../types";

interface Props {
  tab: Tab;
  active: boolean;
  appearance: Appearance;
  layoutKey: string;
  onActivate: (id: string) => void;
}

function encode(value: unknown): string {
  return encodeURIComponent(typeof value === "string" ? value : "");
}

function webviewLabel(tabId: string): string {
  return `collab-terminal-${tabId.replace(/[^a-zA-Z0-9-/:_]/g, "_")}`;
}

function webviewUrl(tab: Tab, appearance: Appearance): string {
  const state = tab.state.kind === "connected" ? tab.state : null;
  const params = new URLSearchParams();
  const bootAppearance = normalizeAppearance(appearance);
  params.set("view", "collab-terminal");
  params.set("tabId", tab.id);
  params.set("sessionId", state?.sessionId ?? "");
  params.set("kind", tab.kind === "local" ? "local" : "ssh");
  params.set("hostId", encode(tab.host.id));
  params.set("label", encode(tab.kind === "local" ? "Terminal" : tab.host.label));
  params.set("hostname", encode(tab.host.hostname));
  params.set("username", encode(tab.host.username));
  params.set("port", String(tab.host.port));
  params.set("os", encode(tab.host.os ?? ""));
  params.set("cwd", encode(tab.localCwd ?? ""));
  params.set("appearance", JSON.stringify(bootAppearance));
  return `/?${params.toString()}`;
}

function rectFor(element: HTMLElement): DOMRect {
  return element.getBoundingClientRect();
}

function clipRectFor(element: HTMLElement): DOMRect {
  return element.closest(".collab-grid")?.getBoundingClientRect() ?? document.documentElement.getBoundingClientRect();
}

function isFullyInside(rect: DOMRect, clip: DOMRect): boolean {
  return rect.top >= clip.top &&
    rect.left >= clip.left &&
    rect.right <= clip.right &&
    rect.bottom <= clip.bottom;
}

type BoundsSnapshot = {
  x: number;
  y: number;
  width: number;
  height: number;
  visible: boolean;
};

export function CollaboratorWebviewTile({ tab, active, appearance, layoutKey, onActivate }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const webviewRef = useRef<Webview | null>(null);
  const lastBoundsRef = useRef<BoundsSnapshot | null>(null);
  const scheduleSyncRef = useRef<() => void>(() => {});
  const [error, setError] = useState<string | null>(null);
  const instanceIdRef = useRef(crypto.randomUUID().replace(/[^a-zA-Z0-9-/:_]/g, "_"));
  const label = useMemo(() => `${webviewLabel(tab.id)}-${instanceIdRef.current}`, [tab.id]);
  const sessionId = tab.state.kind === "connected" ? tab.state.sessionId : null;

  const emitAppearance = () => {
    void emitTo({ kind: "Webview", label }, "collab://appearance", appearance).catch(() => {});
  };

  useEffect(() => {
    if (!sessionId) return;
    const host = hostRef.current;
    if (!host) return;

    let cancelled = false;
    let frame = 0;
    let ro: ResizeObserver | null = null;
    const eventUnlisteners: UnlistenFn[] = [];
    const windowTarget = getCurrentWindow();
    const trackUnlisten = (promise: Promise<UnlistenFn>) => {
      promise.then(unlisten => {
        if (cancelled) unlisten();
        else eventUnlisteners.push(unlisten);
      }).catch(() => {});
    };

    const syncBounds = () => {
      const webview = webviewRef.current;
      const element = hostRef.current;
      if (!webview || !element || cancelled) return;
      const rect = rectFor(element);
      const clip = clipRectFor(element);
      const next: BoundsSnapshot = {
        x: Math.round(rect.left),
        y: Math.round(rect.top),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
        visible: rect.width >= 8 && rect.height >= 8 && isFullyInside(rect, clip),
      };
      const previous = lastBoundsRef.current;

      if (!next.visible) {
        if (previous?.visible !== false) {
          void webview.hide().catch(() => {});
        }
        lastBoundsRef.current = next;
        return;
      }

      const ops: Promise<void>[] = [];
      if (!previous || previous.x !== next.x || previous.y !== next.y) {
        ops.push(webview.setPosition(new LogicalPosition(next.x, next.y)));
      }
      if (!previous || previous.width !== next.width || previous.height !== next.height) {
        ops.push(webview.setSize(new LogicalSize(next.width, next.height)));
      }
      if (previous?.visible !== true) {
        ops.push(webview.show());
      }

      lastBoundsRef.current = next;
      if (ops.length > 0) {
        void Promise.all(ops.map(op => op.catch(() => {})));
      }
    };

    const scheduleSync = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(syncBounds);
    };
    const forceSync = () => {
      lastBoundsRef.current = null;
      void webviewRef.current?.show().catch(() => {});
      scheduleSync();
    };
    scheduleSyncRef.current = scheduleSync;

    const rect = rectFor(host);
    const backgroundColor = findTheme(appearance.themeId).tokens.termBg;
    const webview = new Webview(windowTarget, label, {
      url: webviewUrl(tab, appearance),
      x: Math.max(0, Math.round(rect.left)),
      y: Math.max(0, Math.round(rect.top)),
      width: Math.max(8, Math.round(rect.width)),
      height: Math.max(8, Math.round(rect.height)),
      focus: active,
      acceptFirstMouse: true,
      dragDropEnabled: true,
      backgroundColor,
    });
    webviewRef.current = webview;

    trackUnlisten(webview.once("tauri://created", () => {
      if (cancelled) return;
      setError(null);
      scheduleSync();
      emitAppearance();
      if (active) void webview.setFocus().catch(() => {});
    }));
    trackUnlisten(webview.once("tauri://error", (event) => {
      if (cancelled) return;
      setError(String(event.payload ?? "Unable to create terminal webview"));
    }));

    ro = new ResizeObserver(scheduleSync);
    ro.observe(host);
    const scrollParent = host.closest(".collab-grid");
    const onWindowFocus = () => forceSync();
    const onVisibilityChange = () => {
      if (!document.hidden) forceSync();
    };
    window.addEventListener("resize", scheduleSync);
    window.addEventListener("focus", onWindowFocus);
    document.addEventListener("visibilitychange", onVisibilityChange);
    scrollParent?.addEventListener("scroll", scheduleSync, { passive: true });
    trackUnlisten(windowTarget.onResized(() => forceSync()));
    trackUnlisten(windowTarget.onMoved(() => scheduleSync()));
    trackUnlisten(windowTarget.onFocusChanged(({ payload }) => {
      if (payload) forceSync();
      else scheduleSync();
    }));
    trackUnlisten(windowTarget.onScaleChanged(() => forceSync()));
    scheduleSync();

    return () => {
      cancelled = true;
      cancelAnimationFrame(frame);
      ro?.disconnect();
      for (const unlisten of eventUnlisteners) unlisten();
      scheduleSyncRef.current = () => {};
      lastBoundsRef.current = null;
      window.removeEventListener("resize", scheduleSync);
      window.removeEventListener("focus", onWindowFocus);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      scrollParent?.removeEventListener("scroll", scheduleSync);
      const current = webviewRef.current;
      webviewRef.current = null;
      current?.close().catch(() => {});
    };
  }, [label, sessionId]);

  useEffect(() => {
    scheduleSyncRef.current();
    const t1 = window.setTimeout(() => scheduleSyncRef.current(), 60);
    const t2 = window.setTimeout(() => scheduleSyncRef.current(), 220);
    emitAppearance();
    if (active) {
      webviewRef.current?.setFocus().catch(() => {});
    }
    return () => {
      window.clearTimeout(t1);
      window.clearTimeout(t2);
    };
  }, [active, appearance.themeId, appearance.fontId, appearance.fontSize, layoutKey]);

  return (
    <div
      ref={hostRef}
      className="collab-webview-host"
      onPointerDown={() => onActivate(tab.id)}
    >
      {error && <div className="collab-webview-error">{error}</div>}
    </div>
  );
}
