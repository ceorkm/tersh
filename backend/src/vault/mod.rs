use crate::errors::{AppError, AppResult};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub mod crypto;

/// Local SQLite vault encrypted at rest with AES-256-GCM.
/// Key lives in the OS keychain (service=tersh, account=vault-key-v1).
/// While the app runs, a plaintext runtime copy lives at `vault.runtime.sqlite`
/// (0600 perms). Every mutating method calls `checkpoint()` which re-encrypts
/// the runtime file to `vault.sqlite.enc` via atomic rename.
pub struct Vault {
    conn: Connection,
    key: Zeroizing<[u8; 32]>,
    enc_path: PathBuf,
    runtime_path: PathBuf,
}

impl Vault {
    pub async fn open_default() -> AppResult<Self> {
        let base = data_base_dir()?.join("Tersh");
        tokio::fs::create_dir_all(&base).await?;

        // Single-instance lockfile. Two tersh processes writing to the same
        // encrypted vault concurrently corrupts it (each .checkpoint() races
        // the other's atomic rename). Lockfile holds the PID of the owning
        // process; if that PID is gone, the lock is stale and we take it.
        ensure_single_instance(&base.join("tersh.lock"))?;

        // One-time migration from the previous app dir. If the new
        // Tersh dir has no .enc but the legacy dir does, copy everything across.
        let legacy_base = data_base_dir()?.join(["open", "ter", "mius"].concat());
        let legacy_enc = legacy_base.join("vault.sqlite.enc");
        if !base.join("vault.sqlite.enc").exists() && legacy_enc.exists() {
            for name in [
                "vault.sqlite.enc",
                "vault.sqlite",
                "vault.sqlite.preencrypt.bak",
            ] {
                let src = legacy_base.join(name);
                if src.exists() {
                    let _ = std::fs::copy(&src, base.join(name));
                }
            }
            tracing::warn!(
                "migrated vault data from {} to {}",
                legacy_base.display(),
                base.display()
            );
        }

        let enc_path = base.join("vault.sqlite.enc");
        let runtime_path = base.join("vault.runtime.sqlite");
        let legacy_plain = base.join("vault.sqlite");

        let key = crypto::load_or_create_key()?;

        // First-run / legacy-migration path: if .enc doesn't exist, seed it.
        if !enc_path.exists() {
            if legacy_plain.exists() {
                // Migrate the pre-encryption demo DB without destroying user data.
                let bytes = std::fs::read(&legacy_plain)
                    .map_err(|e| AppError::Vault(format!("read legacy vault: {e}")))?;
                let cipher = crypto::encrypt(&key, &bytes)?;
                write_atomic_0600(&enc_path, &cipher)?;
                let bak = legacy_plain.with_extension("sqlite.preencrypt.bak");
                let _ = std::fs::rename(&legacy_plain, &bak);
                tracing::warn!(
                    "migrated plaintext vault to {}; original kept at {} (delete when ready)",
                    enc_path.display(),
                    bak.display()
                );
            } else {
                // Brand-new install: stage an empty file, encrypt it.
                let cipher = crypto::encrypt(&key, b"")?;
                write_atomic_0600(&enc_path, &cipher)?;
            }
        }

        // Decrypt .enc → runtime file (0600). Empty plaintext means "no DB yet".
        // If the ciphertext cannot decrypt but a valid plaintext runtime DB was
        // left behind by the last run, repair the encrypted snapshot from it.
        let cipher = std::fs::read(&enc_path)
            .map_err(|e| AppError::Vault(format!("read encrypted vault: {e}")))?;
        let plain = if cipher.is_empty() {
            Vec::new()
        } else {
            match crypto::decrypt(&key, &cipher) {
                Ok(plain) => plain,
                Err(e) => {
                    if let Ok(runtime_bytes) = std::fs::read(&runtime_path) {
                        if looks_like_sqlite(&runtime_bytes) {
                            let cipher = crypto::encrypt(&key, &runtime_bytes)?;
                            write_atomic_0600(&enc_path, &cipher)?;
                            tracing::warn!(
                                "encrypted vault could not be decrypted ({e}); repaired it from the runtime sqlite snapshot"
                            );
                            runtime_bytes
                        } else {
                            quarantine_and_seed_empty(&key, &enc_path, e)?
                        }
                    } else {
                        quarantine_and_seed_empty(&key, &enc_path, e)?
                    }
                }
            }
        };
        if !plain.is_empty() {
            write_atomic_0600(&runtime_path, &plain)?;
        } else if runtime_path.exists() {
            let _ = std::fs::remove_file(&runtime_path);
        }

        let conn = Connection::open(&runtime_path)?;
        // Make every commit fsync the main DB file so checkpoint() reads coherent bytes.
        conn.execute_batch(
            "PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;",
        )?;

        let v = Self {
            conn,
            key,
            enc_path,
            runtime_path,
        };
        v.migrate()?;
        // After schema bootstrap, push the encrypted snapshot to disk.
        v.checkpoint()?;
        Ok(v)
    }

    /// Re-encrypt the runtime DB and atomically replace the on-disk ciphertext.
    /// Called after every mutating method below.
    fn checkpoint(&self) -> AppResult<()> {
        self.conn.cache_flush().ok(); // best-effort flush of SQLite page cache
        let bytes = std::fs::read(&self.runtime_path)
            .map_err(|e| AppError::Vault(format!("read runtime db: {e}")))?;
        let cipher = crypto::encrypt(&self.key, &bytes)?;
        write_atomic_0600(&self.enc_path, &cipher)?;
        Ok(())
    }

    fn migrate(&self) -> AppResult<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS hosts (
              id          TEXT PRIMARY KEY,
              label       TEXT NOT NULL,
              hostname    TEXT NOT NULL,
              port        INTEGER NOT NULL DEFAULT 22,
              username    TEXT NOT NULL,
              auth_kind   TEXT NOT NULL,        -- 'password' | 'key_file'
              key_path    TEXT,                 -- for auth_kind='key_file'
              group_name  TEXT,
              os          TEXT,
              created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
              updated_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS keys (
              id           TEXT PRIMARY KEY,
              label        TEXT NOT NULL,
              kind         TEXT NOT NULL,            -- 'ed25519' | 'rsa' | 'ecdsa'
              public_key   TEXT NOT NULL,            -- OpenSSH format public key
              fingerprint  TEXT NOT NULL,            -- SHA256:...
              private_path TEXT,                     -- on-disk path; null if imported into vault
              created_at   INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS known_hosts (
              host_id     TEXT NOT NULL,
              fingerprint TEXT NOT NULL,
              first_seen  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
              PRIMARY KEY (host_id, fingerprint)
            );

            CREATE TABLE IF NOT EXISTS host_secrets (
              host_id    TEXT PRIMARY KEY,
              password   TEXT NOT NULL,
              updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS snippets (
              id          TEXT PRIMARY KEY,
              label       TEXT NOT NULL,
              command     TEXT NOT NULL,
              description TEXT,
              tags        TEXT,                       -- comma-separated
              created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
              updated_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS tunnels (
              id           TEXT PRIMARY KEY,
              host_id      TEXT NOT NULL,
              label        TEXT NOT NULL,
              kind         TEXT NOT NULL,            -- 'local' | 'remote' | 'dynamic'
              local_port   INTEGER NOT NULL,
              remote_host  TEXT,                     -- not used for dynamic
              remote_port  INTEGER,                  -- not used for dynamic
              created_at   INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS session_logs (
              id          TEXT PRIMARY KEY,
              host_id     TEXT NOT NULL,
              started_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
              ended_at    INTEGER,
              bytes_in    INTEGER NOT NULL DEFAULT 0,
              bytes_out   INTEGER NOT NULL DEFAULT 0,
              log_path    TEXT
            );

            -- Generic encrypted key/value store for app-level secrets that
            -- aren't tied to a host (e.g. the prompt-enhancer provider API key).
            -- Inherits the vault's at-rest encryption and dev/prod key split.
            CREATE TABLE IF NOT EXISTS app_secrets (
              name        TEXT PRIMARY KEY,
              value       TEXT NOT NULL,
              updated_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );
            ",
        )?;
        // Lazy migrations
        let _ = self
            .conn
            .execute("ALTER TABLE hosts ADD COLUMN os TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE hosts ADD COLUMN jump_host_id TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE hosts ADD COLUMN env_json TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE hosts ADD COLUMN startup_snippet TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE snippets ADD COLUMN group_path TEXT", []);
        Ok(())
    }

    pub fn list_hosts(&self) -> AppResult<Vec<HostRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, hostname, port, username, auth_kind, key_path, group_name, os, jump_host_id, env_json, startup_snippet FROM hosts ORDER BY group_name, label",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(HostRow {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    hostname: row.get(2)?,
                    port: row.get(3)?,
                    username: row.get(4)?,
                    auth_kind: row.get(5)?,
                    key_path: row.get(6)?,
                    group_name: row.get(7)?,
                    os: row.get(8).ok(),
                    jump_host_id: row.get(9).ok(),
                    env_json: row.get(10).ok(),
                    startup_snippet: row.get(11).ok(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_host(&self, input: AddHostInput) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO hosts (id, label, hostname, port, username, auth_kind, key_path, group_name, os, jump_host_id, env_json, startup_snippet)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                input.label,
                input.hostname,
                input.port,
                input.username,
                input.auth_kind,
                input.key_path,
                input.group_name,
                input.os,
                input.jump_host_id,
                input.env_json,
                input.startup_snippet,
            ],
        )?;
        self.checkpoint()?;
        Ok(id)
    }

    pub fn update_host(&self, id: &str, input: AddHostInput) -> AppResult<()> {
        self.conn.execute(
            "UPDATE hosts SET label=?1, hostname=?2, port=?3, username=?4, auth_kind=?5, key_path=?6, group_name=?7, os=?8, jump_host_id=?9, env_json=?10, startup_snippet=?11, updated_at=strftime('%s','now') WHERE id=?12",
            params![
                input.label,
                input.hostname,
                input.port,
                input.username,
                input.auth_kind,
                input.key_path,
                input.group_name,
                input.os,
                input.jump_host_id,
                input.env_json,
                input.startup_snippet,
                id,
            ],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn delete_host(&self, id: &str) -> AppResult<()> {
        self.conn
            .execute("DELETE FROM known_hosts WHERE host_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM tunnels WHERE host_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM host_secrets WHERE host_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM hosts WHERE id = ?1", params![id])?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn set_host_password(&self, host_id: &str, password: &str) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO host_secrets (host_id, password, updated_at)
             VALUES (?1, ?2, strftime('%s','now'))
             ON CONFLICT(host_id) DO UPDATE SET password=excluded.password, updated_at=excluded.updated_at",
            params![host_id, password],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn get_host_password(&self, host_id: &str) -> AppResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT password FROM host_secrets WHERE host_id = ?1")?;
        match stmt.query_row(params![host_id], |row| row.get::<_, String>(0)) {
            Ok(password) => Ok(Some(password)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn clear_host_password(&self, host_id: &str) -> AppResult<()> {
        self.conn.execute(
            "DELETE FROM host_secrets WHERE host_id = ?1",
            params![host_id],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    /// Store an app-level secret (encrypted at rest with the rest of the
    /// vault). Used for the prompt-enhancer API key so it survives launches.
    pub fn set_app_secret(&self, name: &str, value: &str) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO app_secrets (name, value, updated_at)
             VALUES (?1, ?2, strftime('%s','now'))
             ON CONFLICT(name) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![name, value],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn get_app_secret(&self, name: &str) -> AppResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM app_secrets WHERE name = ?1")?;
        match stmt.query_row(params![name], |row| row.get::<_, String>(0)) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn clear_app_secret(&self, name: &str) -> AppResult<()> {
        self.conn
            .execute("DELETE FROM app_secrets WHERE name = ?1", params![name])?;
        self.checkpoint()?;
        Ok(())
    }

    fn list_host_secrets(&self) -> AppResult<Vec<HostSecretRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT host_id, password, updated_at FROM host_secrets ORDER BY host_id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(HostSecretRow {
                    host_id: row.get(0)?,
                    password: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ─── KEYS ───
    pub fn list_keys(&self) -> AppResult<Vec<KeyRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, kind, public_key, fingerprint, private_path, created_at FROM keys ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(KeyRow {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    kind: row.get(2)?,
                    public_key: row.get(3)?,
                    fingerprint: row.get(4)?,
                    private_path: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_key(&self, input: AddKeyInput) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO keys (id, label, kind, public_key, fingerprint, private_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                input.label,
                input.kind,
                input.public_key,
                input.fingerprint,
                input.private_path
            ],
        )?;
        self.checkpoint()?;
        Ok(id)
    }

    pub fn delete_key(&self, id: &str) -> AppResult<()> {
        self.conn
            .execute("DELETE FROM keys WHERE id = ?1", params![id])?;
        self.checkpoint()?;
        Ok(())
    }

    // ─── SNIPPETS ───
    pub fn list_snippets(&self) -> AppResult<Vec<SnippetRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, command, description, tags, group_path, created_at FROM snippets ORDER BY group_path, label",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SnippetRow {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    command: row.get(2)?,
                    description: row.get(3)?,
                    tags: row.get(4)?,
                    group_path: row.get(5).ok(),
                    created_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_snippet(&self, input: AddSnippetInput) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO snippets (id, label, command, description, tags, group_path) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, input.label, input.command, input.description, input.tags, input.group_path],
        )?;
        self.checkpoint()?;
        Ok(id)
    }

    pub fn update_snippet(&self, id: &str, input: AddSnippetInput) -> AppResult<()> {
        self.conn.execute(
            "UPDATE snippets SET label = ?1, command = ?2, description = ?3, tags = ?4, group_path = ?5, updated_at = strftime('%s','now') WHERE id = ?6",
            params![input.label, input.command, input.description, input.tags, input.group_path, id],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn delete_snippet(&self, id: &str) -> AppResult<()> {
        self.conn
            .execute("DELETE FROM snippets WHERE id = ?1", params![id])?;
        self.checkpoint()?;
        Ok(())
    }

    // ─── KNOWN HOSTS ───
    pub fn list_known_hosts(&self) -> AppResult<Vec<KnownHostRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT host_id, fingerprint, first_seen FROM known_hosts ORDER BY first_seen DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(KnownHostRow {
                    host_id: row.get(0)?,
                    fingerprint: row.get(1)?,
                    first_seen: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// All fingerprints recorded for a given host (typically 0 or 1).
    pub fn known_fingerprints_for(&self, host_id: &str) -> AppResult<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT fingerprint FROM known_hosts WHERE host_id = ?1")?;
        let rows = stmt
            .query_map(params![host_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_known_host(&self, host_id: &str, fingerprint: &str) -> AppResult<()> {
        self.conn.execute(
            "DELETE FROM known_hosts WHERE host_id = ?1 AND fingerprint = ?2",
            params![host_id, fingerprint],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn record_known_host(&self, host_id: &str, fingerprint: &str) -> AppResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO known_hosts (host_id, fingerprint) VALUES (?1, ?2)",
            params![host_id, fingerprint],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    // ─── TUNNELS ───
    pub fn list_tunnels(&self) -> AppResult<Vec<TunnelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, host_id, label, kind, local_port, remote_host, remote_port FROM tunnels ORDER BY label",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TunnelRow {
                    id: row.get(0)?,
                    host_id: row.get(1)?,
                    label: row.get(2)?,
                    kind: row.get(3)?,
                    local_port: row.get(4)?,
                    remote_host: row.get(5)?,
                    remote_port: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn add_tunnel(&self, input: AddTunnelInput) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO tunnels (id, host_id, label, kind, local_port, remote_host, remote_port)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                input.host_id,
                input.label,
                input.kind,
                input.local_port,
                input.remote_host,
                input.remote_port
            ],
        )?;
        self.checkpoint()?;
        Ok(id)
    }

    pub fn delete_tunnel(&self, id: &str) -> AppResult<()> {
        self.conn
            .execute("DELETE FROM tunnels WHERE id = ?1", params![id])?;
        self.checkpoint()?;
        Ok(())
    }

    // ─── SESSION LOGS ───
    pub fn list_session_logs(&self) -> AppResult<Vec<SessionLogRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, host_id, started_at, ended_at, bytes_in, bytes_out, log_path FROM session_logs ORDER BY started_at DESC LIMIT 200",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SessionLogRow {
                    id: row.get(0)?,
                    host_id: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    bytes_in: row.get(4)?,
                    bytes_out: row.get(5)?,
                    log_path: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn record_session_start(&self, id: &str, host_id: &str) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO session_logs (id, host_id) VALUES (?1, ?2)",
            params![id, host_id],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    pub fn record_session_bytes(
        &self,
        id: &str,
        bytes_in_delta: i64,
        bytes_out_delta: i64,
    ) -> AppResult<()> {
        self.conn.execute(
            "UPDATE session_logs
             SET bytes_in = bytes_in + ?1, bytes_out = bytes_out + ?2
             WHERE id = ?3",
            params![bytes_in_delta, bytes_out_delta, id],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    /// Same as `record_session_bytes` but skips the per-call checkpoint.
    /// Callers in async hot paths (SSH byte-flush task) use this to do the
    /// fast SQLite write under the global vault lock, then run the encrypted
    /// snapshot in spawn_blocking AFTER releasing the lock — see
    /// `prepare_checkpoint`. Pre-fix this method was synonymous with
    /// `record_session_bytes` and serialized N session × every 5s blocking
    /// AES-GCM re-encrypts under the lock, stalling the async runtime.
    pub fn record_session_bytes_runtime_only(
        &self,
        id: &str,
        bytes_in_delta: i64,
        bytes_out_delta: i64,
    ) -> AppResult<()> {
        self.conn.execute(
            "UPDATE session_logs
             SET bytes_in = bytes_in + ?1, bytes_out = bytes_out + ?2
             WHERE id = ?3",
            params![bytes_in_delta, bytes_out_delta, id],
        )?;
        Ok(())
    }

    /// Capture the inputs needed to run a checkpoint without holding the
    /// vault lock. Caller drops the lock, then passes these to
    /// `run_checkpoint_blocking` inside a `spawn_blocking`.
    pub fn prepare_checkpoint(&self) -> CheckpointInputs {
        // Best-effort flush of SQLite's page cache so the file read below
        // includes our most-recent writes.
        let _ = self.conn.cache_flush();
        CheckpointInputs {
            runtime_path: self.runtime_path.clone(),
            enc_path: self.enc_path.clone(),
            // The Zeroizing wrapper keeps the key bytes wiped on drop — we
            // re-wrap the copy so the closure's stack copy stays zeroized
            // too once the encrypt returns.
            key: Zeroizing::new(*self.key),
        }
    }

    pub fn record_session_end(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE session_logs
             SET ended_at = COALESCE(ended_at, strftime('%s','now'))
             WHERE id = ?1",
            params![id],
        )?;
        self.checkpoint()?;
        Ok(())
    }

    /// Snapshot every row in the vault as plain JSON. Used by `export_vault`,
    /// which then encrypts the JSON with a user-supplied passphrase.
    pub fn dump_all(&self) -> AppResult<serde_json::Value> {
        let hosts = self.list_hosts()?;
        let keys = self.list_keys()?;
        let snippets = self.list_snippets()?;
        let known = self.list_known_hosts()?;
        let host_secrets = self.list_host_secrets()?;
        let tunnels = self.list_tunnels()?;
        let logs = self.list_session_logs()?;
        Ok(serde_json::json!({
            "version": 1,
            "exported_at": chrono_seconds(),
            "hosts": hosts,
            "keys": keys,
            "snippets": snippets,
            "known_hosts": known,
            "host_secrets": host_secrets,
            "tunnels": tunnels,
            "session_logs": logs,
        }))
    }

    /// Replace all rows from a previously-exported dump. Existing rows are
    /// wiped first — this is a destructive merge by design (import = restore).
    ///
    /// Runs inside a real SQLite transaction: any failure mid-import rolls back
    /// so the user keeps their existing vault. Without this, a malformed import
    /// row would commit the DELETE half and leave the vault corrupted (empty
    /// hosts, partial keys/snippets) — silent data loss.
    pub fn restore_all(&self, dump: &serde_json::Value) -> AppResult<()> {
        validate_restore_dump(dump)?;
        self.conn
            .execute("BEGIN IMMEDIATE", [])
            .map_err(AppError::from)?;
        let result = (|| -> AppResult<()> {
            self.conn.execute_batch(
                "DELETE FROM hosts;
                 DELETE FROM keys;
                 DELETE FROM snippets;
                 DELETE FROM known_hosts;
                 DELETE FROM host_secrets;
                 DELETE FROM tunnels;
                 DELETE FROM session_logs;",
            )?;
            if let Some(arr) = dump.get("hosts").and_then(|v| v.as_array()) {
                for row in arr {
                    let h: HostRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("hosts row: {e}")))?;
                    self.conn.execute(
                        "INSERT INTO hosts (id, label, hostname, port, username, auth_kind, key_path, group_name, os, jump_host_id, env_json, startup_snippet)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            h.id, h.label, h.hostname, h.port, h.username, h.auth_kind,
                            h.key_path, h.group_name, h.os, h.jump_host_id, h.env_json, h.startup_snippet,
                        ],
                    )?;
                }
            }
            if let Some(arr) = dump.get("keys").and_then(|v| v.as_array()) {
                for row in arr {
                    let k: KeyRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("keys row: {e}")))?;
                    self.conn.execute(
                        "INSERT INTO keys (id, label, kind, public_key, fingerprint, private_path, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![k.id, k.label, k.kind, k.public_key, k.fingerprint, k.private_path, k.created_at],
                    )?;
                }
            }
            if let Some(arr) = dump.get("snippets").and_then(|v| v.as_array()) {
                for row in arr {
                    let s: SnippetRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("snippets row: {e}")))?;
                    // group_path MUST be in this insert or every snippet's folder
                    // assignment is silently wiped on import (regression first
                    // shipped when group_path was added to snippets table).
                    self.conn.execute(
                        "INSERT INTO snippets (id, label, command, description, tags, group_path, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![s.id, s.label, s.command, s.description, s.tags, s.group_path, s.created_at],
                    )?;
                }
            }
            if let Some(arr) = dump.get("known_hosts").and_then(|v| v.as_array()) {
                for row in arr {
                    let k: KnownHostRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("known_hosts row: {e}")))?;
                    self.conn.execute(
                        "INSERT INTO known_hosts (host_id, fingerprint, first_seen) VALUES (?1, ?2, ?3)",
                        params![k.host_id, k.fingerprint, k.first_seen],
                    )?;
                }
            }
            if let Some(arr) = dump.get("host_secrets").and_then(|v| v.as_array()) {
                for row in arr {
                    let s: HostSecretRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("host_secrets row: {e}")))?;
                    self.conn.execute(
                        "INSERT INTO host_secrets (host_id, password, updated_at)
                         VALUES (?1, ?2, ?3)",
                        params![s.host_id, s.password, s.updated_at],
                    )?;
                }
            }
            if let Some(arr) = dump.get("tunnels").and_then(|v| v.as_array()) {
                for row in arr {
                    let t: TunnelRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("tunnels row: {e}")))?;
                    self.conn.execute(
                        "INSERT INTO tunnels (id, host_id, label, kind, local_port, remote_host, remote_port)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![t.id, t.host_id, t.label, t.kind, t.local_port, t.remote_host, t.remote_port],
                    )?;
                }
            }
            if let Some(arr) = dump.get("session_logs").and_then(|v| v.as_array()) {
                for row in arr {
                    let s: SessionLogRow = serde_json::from_value(row.clone())
                        .map_err(|e| AppError::Vault(format!("session_logs row: {e}")))?;
                    self.conn.execute(
                        "INSERT INTO session_logs (id, host_id, started_at, ended_at, bytes_in, bytes_out, log_path)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![s.id, s.host_id, s.started_at, s.ended_at, s.bytes_in, s.bytes_out, s.log_path],
                    )?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute("COMMIT", []).map_err(AppError::from)?;
            }
            Err(e) => {
                // Best-effort rollback — if it fails the connection is already
                // in a bad state; we surface the original error either way.
                let _ = self.conn.execute("ROLLBACK", []);
                return Err(e);
            }
        }
        self.checkpoint()?;
        Ok(())
    }
}

fn chrono_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn validate_restore_dump(dump: &serde_json::Value) -> AppResult<()> {
    if dump.get("version").and_then(|v| v.as_i64()) != Some(1) {
        return Err(AppError::Vault(
            "unsupported or missing vault export version".into(),
        ));
    }
    for key in [
        "hosts",
        "keys",
        "snippets",
        "known_hosts",
        "tunnels",
        "session_logs",
    ] {
        if !dump.get(key).is_some_and(|v| v.is_array()) {
            return Err(AppError::Vault(format!(
                "vault export missing array: {key}"
            )));
        }
    }
    if dump.get("host_secrets").is_some_and(|v| !v.is_array()) {
        return Err(AppError::Vault(
            "vault export host_secrets must be an array".into(),
        ));
    }
    Ok(())
}

fn quarantine_path(path: &Path) -> PathBuf {
    let stamp = chrono_seconds();
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("vault.sqlite.enc");
    path.with_file_name(format!("{file_name}.corrupt-{stamp}"))
}

fn looks_like_sqlite(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SQLite format 3\0")
}

fn quarantine_and_seed_empty(key: &[u8; 32], enc_path: &Path, err: AppError) -> AppResult<Vec<u8>> {
    let quarantined = quarantine_path(enc_path);
    std::fs::rename(enc_path, &quarantined).map_err(|rename_err| {
        AppError::Vault(format!(
            "{err}; additionally failed to quarantine {} to {}: {rename_err}",
            enc_path.display(),
            quarantined.display(),
        ))
    })?;
    tracing::error!(
        "encrypted vault could not be decrypted; quarantined {} to {} and starting a new empty vault",
        enc_path.display(),
        quarantined.display()
    );
    let cipher = crypto::encrypt(key, b"")?;
    write_atomic_0600(enc_path, &cipher)?;
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::{AddHostInput, Vault};
    use rusqlite::Connection;
    use zeroize::Zeroizing;

    fn test_vault(name: &str) -> Vault {
        let base =
            std::env::temp_dir().join(format!("tersh-vault-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let runtime_path = base.join("vault.runtime.sqlite");
        let enc_path = base.join("vault.sqlite.enc");
        let conn = Connection::open(&runtime_path).unwrap();
        let vault = Vault {
            conn,
            key: Zeroizing::new([7; 32]),
            enc_path,
            runtime_path,
        };
        vault.migrate().unwrap();
        vault
    }

    fn host_input(label: &str) -> AddHostInput {
        AddHostInput {
            label: label.into(),
            hostname: "example.com".into(),
            port: 22,
            username: "root".into(),
            auth_kind: "password".into(),
            key_path: None,
            group_name: None,
            os: None,
            jump_host_id: None,
            env_json: None,
            startup_snippet: None,
        }
    }

    #[test]
    fn vault_export_restore_preserves_host_secrets() {
        let source = test_vault("source");
        let host_id = source.add_host(host_input("prod")).unwrap();
        source
            .set_host_password(&host_id, "secret-password")
            .unwrap();

        let dump = source.dump_all().unwrap();
        assert_eq!(
            dump["host_secrets"][0]["password"].as_str(),
            Some("secret-password")
        );

        let target = test_vault("target");
        target.restore_all(&dump).unwrap();
        assert_eq!(
            target.get_host_password(&host_id).unwrap().as_deref(),
            Some("secret-password")
        );
    }

    #[test]
    fn malformed_restore_does_not_leave_stale_host_secrets() {
        let vault = test_vault("malformed");
        let host_id = vault.add_host(host_input("old")).unwrap();
        vault.set_host_password(&host_id, "old-secret").unwrap();

        assert!(vault.restore_all(&serde_json::json!({})).is_err());
        assert_eq!(
            vault.get_host_password(&host_id).unwrap().as_deref(),
            Some("old-secret")
        );

        let mut dump = vault.dump_all().unwrap();
        dump["hosts"] = serde_json::json!([]);
        dump["host_secrets"] = serde_json::json!([]);
        vault.restore_all(&dump).unwrap();
        assert_eq!(vault.get_host_password(&host_id).unwrap(), None);
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        // Final encrypted snapshot, then wipe the plaintext runtime file.
        if let Err(e) = self.checkpoint() {
            tracing::error!("vault drop: checkpoint failed: {e}");
        }
        if let Err(e) = std::fs::remove_file(&self.runtime_path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("vault drop: could not remove runtime file: {e}");
            }
        }
        // Release single-instance lockfile.
        if let Some(parent) = self.enc_path.parent() {
            let _ = std::fs::remove_file(parent.join("tersh.lock"));
        }
    }
}

/// Snapshot of everything `run_checkpoint_blocking` needs. Letting callers
/// snapshot under the vault lock and then release it before doing the slow
/// AES-GCM encrypt + atomic file rewrite means a 5s byte-flush across N
/// sessions can no longer queue N blocking re-encrypts under the same lock.
pub struct CheckpointInputs {
    runtime_path: PathBuf,
    enc_path: PathBuf,
    key: Zeroizing<[u8; 32]>,
}

/// Read the runtime SQLite file, AES-GCM encrypt it, and atomically replace
/// the on-disk ciphertext. Pure I/O + CPU — designed to be called inside
/// `tokio::task::spawn_blocking` after the vault Mutex has been released.
pub fn run_checkpoint_blocking(inputs: CheckpointInputs) -> AppResult<()> {
    let bytes = std::fs::read(&inputs.runtime_path)
        .map_err(|e| AppError::Vault(format!("read runtime db: {e}")))?;
    let cipher = crypto::encrypt(&inputs.key, &bytes)?;
    write_atomic_0600(&inputs.enc_path, &cipher)?;
    Ok(())
}

/// Write `bytes` to `path` with 0600 perms via tmp + atomic rename.
fn write_atomic_0600(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("dat"),
        uuid::Uuid::new_v4().simple()
    ));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| AppError::Vault(format!("open tmp {}: {e}", tmp.display())))?;
        f.write_all(bytes)
            .map_err(|e| AppError::Vault(format!("write tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| AppError::Vault(format!("fsync tmp: {e}")))?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::Vault(format!(
            "rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        )));
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct HostRow {
    pub id: String,
    pub label: String,
    pub hostname: String,
    pub port: i64,
    pub username: String,
    pub auth_kind: String,
    pub key_path: Option<String>,
    pub group_name: Option<String>,
    pub os: Option<String>,
    pub jump_host_id: Option<String>,
    pub env_json: Option<String>,
    pub startup_snippet: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct AddHostInput {
    pub label: String,
    pub hostname: String,
    pub port: i64,
    pub username: String,
    pub auth_kind: String,
    pub key_path: Option<String>,
    pub group_name: Option<String>,
    pub os: Option<String>,
    #[serde(default)]
    pub jump_host_id: Option<String>,
    #[serde(default)]
    pub env_json: Option<String>,
    #[serde(default)]
    pub startup_snippet: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct KeyRow {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub public_key: String,
    pub fingerprint: String,
    pub private_path: Option<String>,
    pub created_at: i64,
}

#[derive(serde::Deserialize)]
pub struct AddKeyInput {
    pub label: String,
    pub kind: String,
    pub public_key: String,
    pub fingerprint: String,
    pub private_path: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SnippetRow {
    pub id: String,
    pub label: String,
    pub command: String,
    // `#[serde(default)]` on every optional field so vault imports from
    // older versions (which may not carry these keys at all) still
    // deserialize. Without this, importing a pre-`group_path` export
    // fails with "missing field 'group_path'" and the whole snippets
    // section is silently rolled back.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub group_path: Option<String>,
    pub created_at: i64,
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct AddSnippetInput {
    pub label: String,
    pub command: String,
    pub description: Option<String>,
    pub tags: Option<String>,
    #[serde(default)]
    pub group_path: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct KnownHostRow {
    pub host_id: String,
    pub fingerprint: String,
    pub first_seen: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct HostSecretRow {
    pub host_id: String,
    pub password: String,
    pub updated_at: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TunnelRow {
    pub id: String,
    pub host_id: String,
    pub label: String,
    pub kind: String,
    pub local_port: i64,
    pub remote_host: Option<String>,
    pub remote_port: Option<i64>,
}

#[derive(serde::Deserialize)]
pub struct AddTunnelInput {
    pub host_id: String,
    pub label: String,
    pub kind: String,
    pub local_port: i64,
    pub remote_host: Option<String>,
    pub remote_port: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SessionLogRow {
    pub id: String,
    pub host_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub log_path: Option<String>,
}

/// Refuse to start a second tersh instance if the existing lockfile still
/// points at a live process. The lockfile contains the owning process's PID;
/// a stale PID (process gone) is taken over automatically.
fn ensure_single_instance(lock_path: &Path) -> AppResult<()> {
    let our_pid = std::process::id();

    // If lock exists and the recorded PID is alive AND isn't us, abort.
    if let Ok(contents) = std::fs::read_to_string(lock_path) {
        if let Ok(other_pid) = contents.trim().parse::<u32>() {
            if other_pid != our_pid && pid_is_alive(other_pid) {
                return Err(AppError::Vault(format!(
                    "another tersh instance is already running (pid {other_pid}). \
                     close it, or remove {} if you're sure no tersh is running.",
                    lock_path.display(),
                )));
            }
            // else: stale lock, fall through and overwrite
        }
    }

    // Write our PID. Lockfile is removed on graceful shutdown via Vault::drop.
    std::fs::write(lock_path, our_pid.to_string())
        .map_err(|e| AppError::Vault(format!("write lockfile {}: {e}", lock_path.display())))?;
    Ok(())
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // `kill -0 PID` exits 0 if the process exists and is signalable, non-zero
    // otherwise. Avoids needing the libc crate (which would require a dep
    // review per CLAUDE.md §3.1). One-time startup cost, irrelevant for perf.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    // Conservative on non-Unix: assume alive so we don't blow away a real lock.
    true
}

/// OS-appropriate "user data" base dir. Homegrown to avoid the `directories` crate.
fn data_base_dir() -> AppResult<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| AppError::Vault("$HOME unset".into()))?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg));
        }
        let home = std::env::var("HOME").map_err(|_| AppError::Vault("$HOME unset".into()))?;
        return Ok(PathBuf::from(home).join(".local").join("share"));
    }
    #[cfg(target_os = "windows")]
    {
        let appdata =
            std::env::var("APPDATA").map_err(|_| AppError::Vault("%APPDATA% unset".into()))?;
        return Ok(PathBuf::from(appdata));
    }
    #[allow(unreachable_code)]
    Err(AppError::Vault("unsupported OS".into()))
}
