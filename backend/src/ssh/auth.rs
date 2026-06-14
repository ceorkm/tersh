use crate::errors::{AppError, AppResult};
use russh::keys::{decode_secret_key, key::KeyPair};
use std::fs;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// Expand a leading `~` / `~/` in a key path to the user's home directory.
/// SSH key paths are routinely stored as `~/.ssh/id_ed25519`, but the OS file
/// APIs do not understand `~`, so we resolve it before reading the file.
fn expand_tilde(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if trimmed == "~" || trimmed.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|v| !v.is_empty())
        {
            let rest = trimmed.trim_start_matches('~').trim_start_matches('/');
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(trimmed)
}

/// Recover the actual key path when a user has pasted a full SSH command
/// (e.g. `ssh -i ~/keys/id_ed25519 root@host`) into the key field instead of
/// just the path. We pull the file out of the `-i <path>` argument; otherwise
/// the input is already a plain path and is returned unchanged.
fn key_path_from_input(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(idx) = trimmed.find("-i ") {
        if let Some(token) = trimmed[idx + 3..].split_whitespace().next() {
            return token;
        }
    }
    trimmed
}

/// Load a private SSH key from disk. Handles passphrase-protected keys.
/// Key material is wrapped in `Zeroizing` so it is wiped from RAM when dropped.
pub fn load_secret_key(path: &str, passphrase: Option<&str>) -> AppResult<KeyPair> {
    let resolved = expand_tilde(key_path_from_input(path));
    let bytes = Zeroizing::new(
        fs::read_to_string(&resolved)
            .map_err(|e| AppError::Ssh(format!("read key file {}: {e}", resolved.display())))?,
    );

    decode_secret_key(bytes.as_str(), passphrase)
        .map_err(|e| AppError::Ssh(format!("decode key: {e}")))
}
