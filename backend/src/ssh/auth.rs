use crate::errors::{AppError, AppResult};
use russh::keys::{decode_secret_key, key::KeyPair};
use std::fs;
use zeroize::Zeroizing;

/// Load a private SSH key from disk. Handles passphrase-protected keys.
/// Key material is wrapped in `Zeroizing` so it is wiped from RAM when dropped.
pub fn load_secret_key(path: &str, passphrase: Option<&str>) -> AppResult<KeyPair> {
    let bytes = Zeroizing::new(
        fs::read_to_string(path)
            .map_err(|e| AppError::Ssh(format!("read key file {path}: {e}")))?,
    );

    decode_secret_key(bytes.as_str(), passphrase)
        .map_err(|e| AppError::Ssh(format!("decode key: {e}")))
}
