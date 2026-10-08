//! Linux/Unix credential storage fallback.
use crate::auth::Credentials;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
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
        // Pairing secrets must never be readable by other local users.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
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

#[cfg(test)]
mod tests {
    use super::*;
    fn credentials() -> Credentials {
        Credentials {
            accessory_id: b"synthetic-accessory".to_vec(),
            accessory_public: [7; 32],
            controller_id: b"controller-1".to_vec(),
            controller_secret: [9; 32],
        }
    }
    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("airflash-creds-{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn round_trip_and_missing_read() {
        let root = root("roundtrip");
        let stored = credentials();
        save(&root, "AA:BB:CC:DD:EE:FF", &stored).unwrap();
        let loaded = load(&root, "AA:BB:CC:DD:EE:FF").unwrap().expect("credentials");
        assert_eq!(loaded.accessory_id, stored.accessory_id);
        assert_eq!(loaded.accessory_public, stored.accessory_public);
        assert_eq!(loaded.controller_secret, stored.controller_secret);
        assert!(load(&root, "00:11:22:33:44:55").unwrap().is_none());
        // Colons are irrelevant: the file name is a hash, not the identifier.
        assert!(load(&root, "aabbccddeeff").unwrap().is_some());
        std::fs::remove_dir_all(&root).ok();
    }
    #[test]
    fn tampered_and_foreign_files_are_rejected() {
        let root = root("tamper");
        let stored = credentials();
        save(&root, "AA:BB:CC:DD:EE:FF", &stored).unwrap();
        let target = path(&root, "AA:BB:CC:DD:EE:FF");
        let mut damaged = std::fs::read(&target).unwrap();
        let end = damaged.len() - 1;
        damaged[end] ^= 1;
        std::fs::write(&target, &damaged).unwrap();
        assert!(load(&root, "AA:BB:CC:DD:EE:FF").is_err());
        std::fs::write(&target, b"not-our-format").unwrap();
        assert!(load(&root, "AA:BB:CC:DD:EE:FF").is_err());
        std::fs::write(&target, format!("{}[]", String::from_utf8_lossy(MAGIC)).as_bytes()).unwrap();
        assert!(load(&root, "AA:BB:CC:DD:EE:FF").is_err());
        // Files are created privately and are small.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "credentials must stay private");
        }
        let mut huge = MAGIC.to_vec();
        huge.extend(std::iter::repeat_n(b' ', 16385));
        std::fs::write(&target, &huge).unwrap();
        assert!(load(&root, "AA:BB:CC:DD:EE:FF").is_err());
        std::fs::remove_dir_all(&root).ok();
    }
    #[test]
    fn directory_prefers_xdg_config_home() {
        let _guard = env_lock();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/airflash-xdg-test");
        }
        assert_eq!(directory(), PathBuf::from("/tmp/airflash-xdg-test/AirFlash/native-credentials"));
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
    #[test]
    fn paths_are_hashed_and_stable() {
        let first = path(Path::new("/root"), "AA:BB:CC:DD:EE:FF");
        let second = path(Path::new("/root"), "aabbccddeeff");
        assert_eq!(first, second);
        assert_ne!(first, path(Path::new("/root"), "AA:BB:CC:DD:EE:FE"));
        let name = first.file_name().unwrap().to_string_lossy();
        assert!(name.ends_with(".json"));
        assert_eq!(name.len(), 69, "file name is a SHA-256 hex digest plus .json");
        assert!(name.chars().take(64).all(|c| c.is_ascii_hexdigit()));
        // A hostile identifier cannot escape the storage directory.
        let escaped = path(Path::new("/root"), "../../etc/passwd");
        assert!(escaped.starts_with("/root"));
    }
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }
}
