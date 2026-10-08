//! Linux/Unix credential storage fallback.
use crate::auth::Credentials;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"U2AP\x01";

pub fn directory() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("AirFlash/native-credentials");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/AirFlash/native-credentials")
}

pub fn path(root: &Path, id: &str) -> PathBuf {
    let normalized = id.replace(':', "").to_ascii_lowercase();
    let hash = Sha256::digest(normalized.as_bytes());
    root.join(format!(
        "{}.json",
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

pub fn save(root: &Path, id: &str, credentials: &Credentials) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let clear = Zeroizing::new(serde_json::to_vec(credentials)?);
    let target = path(root, id);
    let temp = root.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(MAGIC)?;
        file.write_all(&clear)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

pub fn load(root: &Path, id: &str) -> Result<Option<Credentials>> {
    let target = path(root, id);
    if !target.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&target)?;
    ensure!(metadata.len() <= 16384, "credential file too large");
    let data = std::fs::read(target)?;
    ensure!(data.starts_with(MAGIC), "unsupported credential format");
    let credentials: Credentials = serde_json::from_slice(&data[MAGIC.len()..])?;
    ensure!(
        !credentials.controller_id.is_empty() && !credentials.accessory_id.is_empty(),
        "empty credential identity"
    );
    Ok(Some(credentials))
}
