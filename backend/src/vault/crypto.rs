use crate::errors::{AppError, AppResult};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use zeroize::Zeroizing;

const KEYCHAIN_SERVICE: &str = "tersh";
const KEYCHAIN_ACCOUNT: &str = "vault-key-v1";

/// Legacy service name from the previous app name. Used only by
/// `migrate_legacy_keychain_key()` to copy the existing key into the new
/// service entry on first run after rename.
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

fn legacy_keychain_service() -> String {
    ["open", "ter", "mius"].concat()
}

pub fn rand_bytes(n: usize) -> AppResult<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| AppError::Vault(format!("os random: {e}")))?;
    Ok(buf)
}

/// Load the vault key from the OS keychain; create + store on first run.
/// Returned key is zeroized on drop.
///
/// DEV vs PROD path split (macOS):
/// - **Release** (`cfg(not(debug_assertions))`): keychain via the `keyring`
///   crate, account `vault-key-v1` under service `tersh`. Codesigned with a
///   real Developer ID at release time, so the keychain ACL is bound to the
///   stable Team ID and persists across launches.
/// - **Debug**: keychain is **not** used. Cargo's debug build produces an
///   ad-hoc-signed binary whose code-signing identifier changes every
///   rebuild (`tersh-<random-hex>`), and macOS keychain ACLs are bound to
///   that identifier. Each rebuild appears as a "different app" — the
///   previous build's keychain item is invisible, so every launch was
///   regenerating a fresh key, breaking decrypt of the existing `.enc`,
///   and triggering recovery from the runtime sqlite snapshot.
///
///   The workaround (stable `--identifier dev.tersh.app` + a
///   `keychain-access-groups` entitlement) requires a Team-ID-prefixed
///   group string. Without an Apple Developer cert, AMFI SIGKILLs binaries
///   that declare it. So in debug builds we side-step the keychain and
///   persist the key to a file under `Library/Application Support/Tersh`
///   instead. This is acceptable in dev only because the runtime sqlite
///   working copy is already plaintext at 0600 — adding a sibling key file
///   at 0600 doesn't make the security model any weaker than it already is.
pub fn load_or_create_key() -> AppResult<Zeroizing<[u8; KEY_BYTES]>> {
    #[cfg(debug_assertions)]
    {
        if let Some(key) = load_or_create_dev_key()? {
            return Ok(key);
        }
        // Fall through to keychain only if the dev path itself errored out
        // in a recoverable way (couldn't resolve data dir, etc.) — keeps
        // the function total instead of bailing on systems where the dev
        // path doesn't apply.
    }
    load_or_create_keychain_key()
}

fn load_or_create_keychain_key() -> AppResult<Zeroizing<[u8; KEY_BYTES]>> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| AppError::Vault(format!("keychain entry: {e}")))?;

    // One-time migration from the previous service name.
    if matches!(entry.get_password(), Err(keyring::Error::NoEntry)) {
        let legacy_service = legacy_keychain_service();
        if let Ok(legacy) = keyring::Entry::new(&legacy_service, KEYCHAIN_ACCOUNT) {
            if let Ok(value) = legacy.get_password() {
                entry
                    .set_password(&value)
                    .map_err(|e| AppError::Vault(format!("migrate keychain value: {e}")))?;
                match legacy.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {
                        tracing::info!("migrated vault key from legacy keychain entry to 'tersh'");
                    }
                    Err(e) => tracing::warn!(
                        "migrated vault key but could not delete legacy keychain entry: {e}"
                    ),
                }
            }
        }
    }

    match entry.get_password() {
        Ok(b64) => {
            let bytes = b64_decode(b64.trim())
                .map_err(|e| AppError::Vault(format!("decode keychain value: {e}")))?;
            if bytes.len() != KEY_BYTES {
                return Err(AppError::Vault(format!(
                    "keychain key length wrong: got {}, want {}",
                    bytes.len(),
                    KEY_BYTES
                )));
            }
            let mut out = [0u8; KEY_BYTES];
            out.copy_from_slice(&bytes);
            Ok(Zeroizing::new(out))
        }
        Err(keyring::Error::NoEntry) => {
            let fresh = rand_bytes(KEY_BYTES)?;
            entry
                .set_password(&b64_encode(&fresh))
                .map_err(|e| AppError::Vault(format!("store keychain value: {e}")))?;
            let mut out = [0u8; KEY_BYTES];
            out.copy_from_slice(&fresh);
            Ok(Zeroizing::new(out))
        }
        Err(e) => Err(AppError::Vault(format!("keychain read: {e}"))),
    }
}

/// Debug-only file-backed key. Lives next to the vault DB at
/// `Library/Application Support/Tersh/.dev-vault-key.b64` (mode 0600).
/// Returns `Ok(None)` if the data dir can't be resolved — callers fall
/// back to the keychain path in that case.
#[cfg(debug_assertions)]
fn load_or_create_dev_key() -> AppResult<Option<Zeroizing<[u8; KEY_BYTES]>>> {
    use std::io::Write;
    use std::path::PathBuf;

    let Some(dir) = dev_key_dir() else {
        return Ok(None);
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            "dev key: could not create {} ({e}); falling back to keychain",
            dir.display()
        );
        return Ok(None);
    }
    let path: PathBuf = dir.join(".dev-vault-key.b64");

    // Read existing key if present.
    if let Ok(b64) = std::fs::read_to_string(&path) {
        let bytes =
            b64_decode(b64.trim()).map_err(|e| AppError::Vault(format!("decode dev key: {e}")))?;
        if bytes.len() == KEY_BYTES {
            let mut out = [0u8; KEY_BYTES];
            out.copy_from_slice(&bytes);
            return Ok(Some(Zeroizing::new(out)));
        }
        // File exists but is malformed — overwrite with a fresh key rather
        // than refusing to start. Quarantining is overkill for a dev-only
        // sidecar.
        tracing::warn!(
            "dev key at {} is malformed (len {}); regenerating",
            path.display(),
            bytes.len()
        );
    }

    // Create a fresh key, write it 0600.
    let fresh = rand_bytes(KEY_BYTES)?;
    let encoded = b64_encode(&fresh);
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&path)
            .map_err(|e| AppError::Vault(format!("open dev key {}: {e}", path.display())))?;
        f.write_all(encoded.as_bytes())
            .map_err(|e| AppError::Vault(format!("write dev key: {e}")))?;
        f.sync_all()
            .map_err(|e| AppError::Vault(format!("fsync dev key: {e}")))?;
    }
    let mut out = [0u8; KEY_BYTES];
    out.copy_from_slice(&fresh);
    tracing::info!("dev key: generated fresh key at {}", path.display());
    Ok(Some(Zeroizing::new(out)))
}

#[cfg(debug_assertions)]
fn dev_key_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = std::path::PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Tersh");
    Some(p)
}

/// AES-256-GCM with a random 12-byte nonce. Output: nonce || ciphertext+tag.
pub fn encrypt(key: &[u8; KEY_BYTES], plaintext: &[u8]) -> AppResult<Vec<u8>> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| AppError::Vault(format!("cipher init: {e}")))?;
    let nonce_bytes = rand_bytes(NONCE_BYTES)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| AppError::Vault("aes-gcm encrypt failed".into()))?;
    let mut out = Vec::with_capacity(NONCE_BYTES + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt(key: &[u8; KEY_BYTES], blob: &[u8]) -> AppResult<Vec<u8>> {
    if blob.len() < NONCE_BYTES + 16 {
        return Err(AppError::Vault("ciphertext too short".into()));
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_BYTES);
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| AppError::Vault(format!("cipher init: {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|_| {
        AppError::Vault("aes-gcm decrypt failed (wrong key, corrupt file, or tampered)".into())
    })
}

/// Derive a 32-byte AES key from a user passphrase via Argon2id.
/// Uses Argon2id default params (m_cost=19 MiB, t_cost=2, p=1) which is
/// the OWASP-recommended baseline for interactive auth.
pub fn derive_key_from_passphrase(
    passphrase: &str,
    salt: &[u8],
) -> AppResult<Zeroizing<[u8; KEY_BYTES]>> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(19_456, 2, 1, Some(KEY_BYTES))
        .map_err(|e| AppError::Vault(format!("argon2 params: {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; KEY_BYTES];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| AppError::Vault(format!("argon2 derive: {e}")))?;
    Ok(Zeroizing::new(out))
}

/// Envelope format used by vault export/import: base64-tagged JSON wrapping
/// algo, salt, nonce, ciphertext. Tied to passphrase, not the keychain key.
pub fn passphrase_encrypt(passphrase: &str, plaintext: &[u8]) -> AppResult<String> {
    let salt = rand_bytes(16)?;
    let key = derive_key_from_passphrase(passphrase, &salt)?;
    let nonce_bytes = rand_bytes(NONCE_BYTES)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref())
        .map_err(|e| AppError::Vault(format!("cipher init: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| AppError::Vault("aes-gcm encrypt failed".into()))?;
    let envelope = serde_json::json!({
        "format": "tersh-vault-export",
        "version": 1,
        "kdf": "argon2id-19456-2-1",
        "cipher": "aes-256-gcm",
        "salt": b64_encode(&salt),
        "nonce": b64_encode(&nonce_bytes),
        "ciphertext": b64_encode(&ct),
    });
    Ok(serde_json::to_string_pretty(&envelope).unwrap_or_default())
}

pub fn passphrase_decrypt(passphrase: &str, envelope_json: &str) -> AppResult<Vec<u8>> {
    let env: serde_json::Value = serde_json::from_str(envelope_json)
        .map_err(|e| AppError::Vault(format!("envelope parse: {e}")))?;
    let salt_b64 = env
        .get("salt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Vault("missing salt".into()))?;
    let nonce_b64 = env
        .get("nonce")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Vault("missing nonce".into()))?;
    let ct_b64 = env
        .get("ciphertext")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Vault("missing ciphertext".into()))?;
    let salt = b64_decode(salt_b64).map_err(|e| AppError::Vault(format!("salt: {e}")))?;
    let nonce_bytes = b64_decode(nonce_b64).map_err(|e| AppError::Vault(format!("nonce: {e}")))?;
    let ct = b64_decode(ct_b64).map_err(|e| AppError::Vault(format!("ciphertext: {e}")))?;
    let key = derive_key_from_passphrase(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref())
        .map_err(|e| AppError::Vault(format!("cipher init: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    cipher
        .decrypt(nonce, ct.as_ref())
        .map_err(|_| AppError::Vault("decrypt failed (wrong passphrase or corrupt file)".into()))
}

// ── tiny base64 (no external dep) ─────────────────────────────────────────────
// Standard alphabet, with `=` padding.

const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHA[(b0 >> 2) as usize] as char);
        out.push(ALPHA[(((b0 << 4) & 0x30) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHA[(((b1 << 2) & 0x3c) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHA[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("bad base64 char: {:?}", c as char)),
        }
    }
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err("base64 length not multiple of 4".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        let c0 = val(chunk[0])?;
        let c1 = val(chunk[1])?;
        let c2 = if pad >= 2 { 0 } else { val(chunk[2])? };
        let c3 = if pad >= 1 { 0 } else { val(chunk[3])? };
        out.push((c0 << 2) | (c1 >> 4));
        if pad < 2 {
            out.push(((c1 & 0x0f) << 4) | (c2 >> 2));
        }
        if pad < 1 {
            out.push(((c2 & 0x03) << 6) | c3);
        }
    }
    Ok(out)
}
