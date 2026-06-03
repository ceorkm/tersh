#!/usr/bin/env bash
# Cargo runner wrapper — re-signs the dev binary with a STABLE ad-hoc
# code-signing identifier before execution.
#
# Why this exists
# ---------------
# Cargo's default debug build assigns the binary an identifier of the form
# `tersh-<random-hex>`, where the hex changes on every rebuild. macOS keychain
# ACLs bind to the code-signing identity + identifier — so each rebuild looks
# like a "different app" to macOS, and keychain items written by the previous
# build come back as `errSecItemNotFound` to the new one.
#
# That broke the encrypted-vault flow: `load_or_create_key` would fall into
# the `NoEntry` arm on every rebuild, generate a fresh random key, store it,
# and then fail to decrypt the existing `vault.sqlite.enc` (which was encrypted
# under the OLD key). The runtime-snapshot recovery kicks in and the user sees
# a "encrypted vault could not be decrypted; repaired from runtime sqlite"
# warning every launch.
#
# Production builds with a real Apple Developer ID don't have this problem:
# the keychain ACL binds to the Team ID, which is stable across rebuilds.
# This script gives the DEV binary a stable identifier so the same code path
# works the same way in dev as in production.
#
# Wired in via backend/.cargo/config.toml as the cargo `runner` for the macOS
# targets. Triggered automatically on `cargo run` (which is what `tauri dev`
# invokes internally).

set -euo pipefail

BIN="$1"
shift || true

# Only re-sign on macOS, and only the tersh binary itself (not cargo test
# binaries, build scripts, or examples). Stable --identifier keeps the
# codesign hash bound to a fixed name across rebuilds, which avoids the
# `tersh-<random-hex>` churn cargo's default linker-signing produces.
#
# We deliberately do NOT pass --entitlements here: `keychain-access-groups`
# requires a Team-ID-prefixed group string, and ad-hoc signed binaries
# without an Apple Developer ID get SIGKILLed by AMFI when they declare it.
# Keychain persistence across launches is handled instead by the dev-mode
# file-backed key path in backend/src/vault/crypto.rs (debug_assertions),
# so the binary doesn't need keychain access at all in dev. Release builds
# get their stable identity from the real Developer ID signature and use
# the keychain normally.
if [[ "$OSTYPE" == "darwin"* ]] && [[ "$(basename "$BIN")" == "tersh" ]]; then
  codesign \
    --sign - \
    --identifier dev.tersh.app \
    --force \
    "$BIN" 2>/dev/null || true
fi

exec "$BIN" "$@"
