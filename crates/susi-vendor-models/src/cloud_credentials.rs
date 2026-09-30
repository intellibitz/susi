//! Preserve multiple user-supplied keys per vendor, using owner-only records.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

#[derive(Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    pub vendor_env: String,
    key: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("vendor_env", &self.vendor_env)
            .finish_non_exhaustive()
    }
}

impl Credential {
    pub fn key(&self) -> &str {
        &self.key
    }
}

pub fn directory() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("cloud-credentials")
}

pub fn save(dir: &Path, vendor_env: &str, key: &str) -> EaiResult<String> {
    if key.is_empty()
        || key.len() > 8192
        || key.chars().any(char::is_control)
        || vendor_env.is_empty()
        || vendor_env.len() > 128
        || !vendor_env
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(EaiError::config("invalid cloud credential"));
    }
    let _lock = susi_config::file_lock::FileLock::acquire(dir, "credentials")
        .ok_or_else(|| EaiError::io("credential store lock unavailable"))?;
    let mut hash = Sha256::new();
    hash.update(vendor_env.as_bytes());
    hash.update([0]);
    hash.update(key.as_bytes());
    let id = hex::encode(hash.finalize());
    let path = dir.join(format!("{id}.json"));
    if !path.exists()
        && std::fs::read_dir(dir)
            .map_err(|e| EaiError::io(e.to_string()))?
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "json"))
            .count()
            >= 1024
    {
        return Err(EaiError::config("cloud credential inventory is full"));
    }
    let entry = Credential {
        id: id.clone(),
        vendor_env: vendor_env.into(),
        key: key.into(),
    };
    let bytes =
        serde_json::to_vec(&entry).map_err(|_| EaiError::config("cannot encode credential"))?;
    susi_config::atomic_write_bytes(&path, &bytes).map_err(|e| EaiError::io(e.to_string()))?;
    Ok(id)
}

pub fn list(dir: &Path, vendor_env: &str) -> EaiResult<Vec<Credential>> {
    let files = match std::fs::read_dir(dir) {
        Ok(files) => files,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(EaiError::io(e.to_string())),
    };
    let mut records = Vec::new();
    for file in files {
        let file = file.map_err(|e| EaiError::io(e.to_string()))?;
        if file.path().extension().is_none_or(|e| e != "json") {
            continue;
        }
        if records.len() >= 1024
            || file
                .metadata()
                .map_err(|e| EaiError::io(e.to_string()))?
                .len()
                > 16 * 1024
        {
            return Err(EaiError::config("credential inventory exceeds limit"));
        }
        let bytes = std::fs::read(file.path()).map_err(|e| EaiError::io(e.to_string()))?;
        let record: Credential = serde_json::from_slice(&bytes)
            .map_err(|_| EaiError::config("invalid credential inventory"))?;
        if record.vendor_env == vendor_env {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(records)
}

/// Explicit user removal only; inference failures never call this.
pub fn remove_vendor(dir: &Path, vendor_env: &str) -> EaiResult<()> {
    let _lock = susi_config::file_lock::FileLock::acquire(dir, "credentials")
        .ok_or_else(|| EaiError::io("credential store lock unavailable"))?;
    for record in list(dir, vendor_env)? {
        if record.id.len() != 64 || !record.id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EaiError::config("invalid credential id"));
        }
        std::fs::remove_file(dir.join(format!("{}.json", record.id)))
            .map_err(|e| EaiError::io(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_quota_inventory_preserves_multiple_keys_with_opaque_ids() {
        let root = tempfile::tempdir().unwrap();
        let a = save(root.path(), "ANTHROPIC_API_KEY", "canary-one").unwrap();
        let b = save(root.path(), "ANTHROPIC_API_KEY", "canary-two").unwrap();
        assert_ne!(a, b);
        assert!(!a.contains("canary"));
        assert_eq!(
            save(root.path(), "ANTHROPIC_API_KEY", "canary-one").unwrap(),
            a
        );
        let entries = list(root.path(), "ANTHROPIC_API_KEY").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!format!("{entries:?}").contains("canary"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join(format!("{a}.json")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        remove_vendor(root.path(), "ANTHROPIC_API_KEY").unwrap();
        assert!(list(root.path(), "ANTHROPIC_API_KEY").unwrap().is_empty());
    }
}
