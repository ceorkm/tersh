use crate::brain::{BrainId, BrainMeta};
use crate::errors::{AppError, AppResult};
use std::path::{Path, PathBuf};

pub fn brain_root_dir() -> AppResult<PathBuf> {
    Ok(data_base_dir()?.join("Tersh").join("brain"))
}

pub fn brain_dir(id: &BrainId) -> AppResult<PathBuf> {
    Ok(brain_root_dir()?.join(&id.0))
}

pub fn meta_path(id: &BrainId) -> AppResult<PathBuf> {
    Ok(brain_dir(id)?.join("meta.json"))
}

pub fn index_path(id: &BrainId) -> AppResult<PathBuf> {
    Ok(brain_dir(id)?.join("index.json"))
}

pub async fn write_meta(meta: &BrainMeta) -> AppResult<()> {
    let path = meta_path(&meta.id)?;
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|e| AppError::Internal(format!("encode meta: {e}")))?;
    write_atomic(&path, &bytes).await
}

pub async fn write_atomic(path: &Path, bytes: &[u8]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Internal(format!("mkdir parent: {e}")))?;
    }
    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("dat"),
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| AppError::Internal(format!("write tmp {}: {e}", tmp.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
    }
    tokio::fs::rename(&tmp, path).await.map_err(|e| {
        AppError::Internal(format!(
            "rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        ))
    })?;
    Ok(())
}

fn data_base_dir() -> AppResult<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| AppError::Internal("$HOME unset".into()))?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg));
        }
        let home = std::env::var("HOME").map_err(|_| AppError::Internal("$HOME unset".into()))?;
        return Ok(PathBuf::from(home).join(".local").join("share"));
    }
    #[cfg(target_os = "windows")]
    {
        let appdata =
            std::env::var("APPDATA").map_err(|_| AppError::Internal("%APPDATA% unset".into()))?;
        return Ok(PathBuf::from(appdata));
    }
    #[allow(unreachable_code)]
    Err(AppError::Internal("unsupported OS".into()))
}
