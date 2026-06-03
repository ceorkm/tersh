import { useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { TerminalView } from "./TerminalView";
import {
  applyAppearance,
  loadAppearance,
  normalizeAppearance,
  type Appearance,
} from "../lib/appearance";
import type { HostRow, Tab, UploadResult } from "../types";

function param(name: string): string {
  return new URLSearchParams(window.location.search).get(name) ?? "";
}

function decodeParam(name: string): string {
  const value = param(name);
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function tabFromUrl(): Tab | null {
  const tabId = param("tabId");
  const sessionId = param("sessionId");
  if (!tabId || !sessionId) return null;

  const kind = param("kind") === "local" ? "local" : "ssh";
  const host: HostRow = {
    id: decodeParam("hostId") || (kind === "local" ? "local-terminal" : tabId),
    label: decodeParam("label") || "Terminal",
    hostname: decodeParam("hostname") || (kind === "local" ? "localhost" : ""),
    port: Number(param("port")) || (kind === "local" ? 0 : 22),
    username: decodeParam("username") || (kind === "local" ? "local" : ""),
    auth_kind: "password",
    key_path: null,
    group_name: null,
    os: (decodeParam("os") as HostRow["os"]) || (kind === "local" ? "apple" : "linux"),
  };

  return {
    id: tabId,
    host,
    kind,
    localCwd: decodeParam("cwd") || undefined,
    state: { kind: "connected", sessionId },
  };
}

function appearanceFromUrl(): Appearance {
  const raw = param("appearance");
  if (!raw) return loadAppearance();
  try {
    return normalizeAppearance(JSON.parse(raw));
  } catch {
    return loadAppearance();
  }
}

export function CollaboratorTerminalGuest() {
  const tab = useMemo(tabFromUrl, []);
  const [appearance, setAppearance] = useState<Appearance>(() => appearanceFromUrl());
  const pendingWheelRef = useRef<{ deltaX: number; deltaY: number; shiftKey: boolean } | null>(null);
  const wheelFrameRef = useRef(0);
  const currentWebview = useMemo(() => {
    try {
      return getCurrentWebview();
    } catch {
      return null;
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlistenTauri: (() => void) | null = null;
    currentWebview
      ?.listen<Appearance>("collab://appearance", event => {
        setAppearance(normalizeAppearance(event.payload));
      })
      .then(unlisten => {
        if (cancelled) unlisten();
        else unlistenTauri = unlisten;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlistenTauri?.();
    };
  }, [currentWebview]);

  useEffect(() => {
    applyAppearance(appearance);
  }, [appearance]);

  useEffect(() => {
    const onWheel = (event: WheelEvent) => {
      const horizontalIntent = Math.abs(event.deltaX) > Math.abs(event.deltaY) || event.shiftKey;
      if (!horizontalIntent) return;
      const pending = pendingWheelRef.current;
      pendingWheelRef.current = {
        deltaX: (pending?.deltaX ?? 0) + event.deltaX,
        deltaY: (pending?.deltaY ?? 0) + event.deltaY,
        shiftKey: event.shiftKey || Boolean(pending?.shiftKey),
      };
      if (wheelFrameRef.current) return;
      wheelFrameRef.current = window.requestAnimationFrame(() => {
        wheelFrameRef.current = 0;
        const payload = pendingWheelRef.current;
        pendingWheelRef.current = null;
        if (payload) emitToMain("collab://terminal-wheel", payload);
      });
    };
    window.addEventListener("wheel", onWheel, { passive: true });
    return () => {
      window.removeEventListener("wheel", onWheel);
      if (wheelFrameRef.current) {
        window.cancelAnimationFrame(wheelFrameRef.current);
        wheelFrameRef.current = 0;
      }
      pendingWheelRef.current = null;
    };
  }, [currentWebview]);

  if (!tab || tab.state.kind !== "connected") {
    return <div className="collab-guest-error">Missing terminal session.</div>;
  }

  const emitToMain = (event: string, payload: unknown) => {
    currentWebview?.emitTo("main", event, payload).catch(() => {});
  };

  return (
    <div className="collab-guest-shell">
      <TerminalView
        tab={tab}
        appearance={appearance}
        drawerOpen={false}
        allowUploads={tab.kind !== "local"}
        onFocusRequest={() => emitToMain("collab://terminal-focus", { tabId: tab.id })}
        onPendingChip={(chip: UploadResult | undefined) => {
          emitToMain("collab://pending-chip", { tabId: tab.id, chip });
        }}
      />
    </div>
  );
}
