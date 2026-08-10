#!/usr/bin/env bash
# Regression guard — encodes invariants that have been silently broken before.
# Each check names the incident it prevents. If a check fails, the fix is to
# understand why the invariant exists (see kratos memory / CLAUDE.md), not to
# delete the check. Run: npm run guard (also enforced in CI).
set -u
cd "$(dirname "$0")/.."
fail=0
err() { echo "GUARD FAIL: $1"; fail=1; }

TV=frontend/src/components/TerminalView.tsx

# 1. Sticky-typing regression (2026-05-21, reintroduced 2026-05-29): any
#    diagnostic IPC in the keystroke hot path (flushInput/queueInput/onData)
#    makes typing laggy. One-shot diagLog calls elsewhere are fine.
awk '
  /const (flushInput|queueInput) = |term\.onData\(/ { infn=1; depth=0; opened=0 }
  infn {
    if (/diagLog/) found=1
    depth += gsub(/\{/,"{") - gsub(/\}/,"}")
    if (depth > 0) opened=1
    if (opened && depth <= 0) infn=0
  }
  END { exit !found }' "$TV" \
  && err "diagLog inside flushInput/queueInput/onData — per-keystroke IPC caused the sticky-typing regression twice"

# 2. Copy-while-Claude-Code regression (2026-08-10): apps must never capture
#    the mouse; drag-select must always work.
grep -q "MOUSE_TRACKING_MODES" "$TV" || err "mouse-tracking DECSET block removed — Claude Code will capture the mouse and kill text selection/copy"
grep -q "macOptionClickForcesSelection: true" "$TV" || err "macOptionClickForcesSelection no longer true — no selection fallback if an app enables mouse mode"

# 3. OSC 52 security posture (2026-08-10): remote clipboard writes are
#    per-host opt-in, reads always blocked.
grep -q "registerOscHandler(52" "$TV" || err "OSC 52 handler removed — remote clipboard sequences would hit xterm defaults"
grep -q "allowRemoteClipboardRef" "$TV" || err "OSC 52 handler no longer gated on per-host allow_remote_clipboard opt-in"

# 4. Typing latency over SSH (2026-08-10): Nagle must stay off.
grep -q "set_nodelay(true)" backend/src/ssh/mod.rs || err "TCP_NODELAY removed from SSH connect — Nagle adds 40ms+ keystroke stalls"

# 5. Supply-chain posture (CLAUDE.md §2.1): exact pins only, no lifecycle
#    scripts in our own package.json.
grep -nE '"[~^][0-9]' package.json && err "floating version range in package.json — exact pins only"
grep -nE '"(pre|post)install"|"prepare"|"prepublish"' package.json && err "lifecycle script in package.json — forbidden (§2.1 rule 3)"

# 6. Resize-artifact regression (2026-05-31): fit() must stay debounced; a
#    fit call inside a rAF loop interleaves with agent TUI redraws.
[ "$(grep -c "fitAndResize(" "$TV")" -gt 12 ] && err "fitAndResize call count grew — check none were added inside animation frames (2026-05-31 artifact regression)"

if [ "$fail" -eq 0 ]; then echo "guard: all invariants hold"; else exit 1; fi
