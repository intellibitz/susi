//! Preserve multiple user-supplied keys per vendor, using owner-only records,
//! optional per-key spend caps, and obfuscated/encrypted storage.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

#[derive(Serialize, Deserialize, Clone)]
pub struct Credential {
    pub id: String,
    pub vendor_env: String,
    key: String,
    #[serde(default)]
    pub max_budget_usd: Option<f64>,
    #[serde(default)]
    pub current_spend_usd: Option<f64>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub encrypted: Option<bool>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("vendor_env", &self.vendor_env)
            .field("max_budget_usd", &self.max_budget_usd)
            .field("current_spend_usd", &self.current_spend_usd)
            .field("label", &self.label)
            .field("masked_key", &self.mask_key())
            .finish()
    }
}

impl Credential {
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Decrypt `key` via the enclosing config dir's vault key when sealed (`enc:v1:`),
    /// or return legacy plaintext directly.
    pub fn unseal_key(&self, dir: &Path) -> EaiResult<String> {
        let vault_dir = dir.parent().unwrap_or(dir);
        susi_config::vault_store::unseal_credential_secret(vault_dir, &self.key)
            .map_err(EaiError::config)
    }

    /// Returns a masked key preview (e.g. `sk-a...1234`) safe for CLI listing.
    pub fn mask_key(&self) -> String {
        let k = self.key.trim();
        if k.starts_with(susi_config::vault_store::SEALED_VALUE_PREFIX) {
            return "[sealed]".to_string();
        }
        if k.len() <= 8 {
            "***".to_string()
        } else {
            format!("{}...{}", &k[..4], &k[k.len() - 4..])
        }
    }

    /// True when the credential has a budget cap and current spend meets or exceeds it.
    pub fn is_budget_exceeded(&self) -> bool {
        match (self.max_budget_usd, self.current_spend_usd) {
            (Some(max), Some(current)) => current >= max,
            _ => false,
        }
    }
}

pub fn directory() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("cloud-credentials")
}

pub fn save(dir: &Path, vendor_env: &str, key: &str) -> EaiResult<String> {
    save_with_options(dir, vendor_env, key, None, None)
}

pub fn save_with_options(
    dir: &Path,
    vendor_env: &str,
    key: &str,
    max_budget_usd: Option<f64>,
    label: Option<String>,
) -> EaiResult<String> {
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
    let id = susi_paths::hex::encode(hash.finalize());
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

    // Preserve existing spend if updating
    let existing_spend = if path.exists() {
        if let Ok(bytes) = std::fs::read(&path) {
            serde_json::from_slice::<Credential>(&bytes)
                .ok()
                .and_then(|c| c.current_spend_usd)
        } else {
            None
        }
    } else {
        None
    };

    let vault_dir = dir.parent().unwrap_or(dir);
    let (stored_key, is_encrypted) =
        match susi_config::vault_store::seal_credential_secret(vault_dir, key) {
            Ok(Some(sealed)) => (sealed, true),
            Ok(None) | Err(_) => (key.into(), false),
        };

    let entry = Credential {
        id: id.clone(),
        vendor_env: vendor_env.into(),
        key: stored_key,
        max_budget_usd,
        current_spend_usd: existing_spend.or(Some(0.0)),
        label,
        encrypted: Some(is_encrypted),
    };

    let bytes =
        serde_json::to_vec(&entry).map_err(|_| EaiError::config("cannot encode credential"))?;
    susi_config::atomic_write_bytes(&path, &bytes).map_err(|e| EaiError::io(e.to_string()))?;
    Ok(id)
}

pub fn list(dir: &Path, vendor_env: &str) -> EaiResult<Vec<Credential>> {
    let all = list_all(dir)?;
    Ok(all
        .into_iter()
        .filter(|c| c.vendor_env == vendor_env)
        .collect())
}

pub fn list_all(dir: &Path) -> EaiResult<Vec<Credential>> {
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
        records.push(record);
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(records)
}

pub fn get_by_id(dir: &Path, id: &str) -> EaiResult<Option<Credential>> {
    let path = dir.join(format!("{id}.json"));
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| EaiError::io(e.to_string()))?;
    let record: Credential = serde_json::from_slice(&bytes)
        .map_err(|_| EaiError::config("invalid credential inventory record"))?;
    Ok(Some(record))
}

/// Record expenditure against a specific credential ID, incrementing current_spend_usd.
pub fn record_spend(dir: &Path, id: &str, amount_usd: f64) -> EaiResult<f64> {
    if amount_usd < 0.0 {
        return Err(EaiError::config("spend amount must be non-negative"));
    }
    let _lock = susi_config::file_lock::FileLock::acquire(dir, "credentials")
        .ok_or_else(|| EaiError::io("credential store lock unavailable"))?;

    let path = dir.join(format!("{id}.json"));
    if !path.exists() {
        return Err(EaiError::config("credential record not found"));
    }

    let bytes = std::fs::read(&path).map_err(|e| EaiError::io(e.to_string()))?;
    let mut record: Credential = serde_json::from_slice(&bytes)
        .map_err(|_| EaiError::config("invalid credential record"))?;

    let new_spend = record.current_spend_usd.unwrap_or(0.0) + amount_usd;
    record.current_spend_usd = Some(new_spend);

    let new_bytes =
        serde_json::to_vec(&record).map_err(|_| EaiError::config("cannot encode credential"))?;
    susi_config::atomic_write_bytes(&path, &new_bytes).map_err(|e| EaiError::io(e.to_string()))?;

    Ok(new_spend)
}

/// Explicit user removal only; inference failures never call this.
pub fn remove_vendor(dir: &Path, vendor_env: &str) -> EaiResult<()> {
    let _lock = susi_config::file_lock::FileLock::acquire(dir, "credentials")
        .ok_or_else(|| EaiError::io("credential store lock unavailable"))?;
    for record in list(dir, vendor_env)? {
        if record.id.len() != 64 || !record.id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EaiError::config("invalid credential id"));
        }
        let _ = std::fs::remove_file(dir.join(format!("{}.json", record.id)));
    }
    Ok(())
}

pub fn remove_by_id(dir: &Path, id: &str) -> EaiResult<()> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(EaiError::config("invalid credential id"));
    }
    let _lock = susi_config::file_lock::FileLock::acquire(dir, "credentials")
        .ok_or_else(|| EaiError::io("credential store lock unavailable"))?;
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| EaiError::io(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_quota_inventory_preserves_multiple_keys_with_opaque_ids() {
        let root = susi_paths::tempfile::tempdir().unwrap();
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
        remove_vendor(root.path(), "ANTHROPIC_API_KEY").unwrap();
        assert!(list(root.path(), "ANTHROPIC_API_KEY").unwrap().is_empty());
    }

    #[test]
    fn credential_budget_and_spend_caps_work() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        let id = save_with_options(
            root.path(),
            "OPENAI_API_KEY",
            "sk-test1234567890",
            Some(10.0),
            Some("prod-key".into()),
        )
        .unwrap();

        let cred = get_by_id(root.path(), &id).unwrap().unwrap();
        assert_eq!(cred.mask_key(), "sk-t...7890");
        assert!(!cred.is_budget_exceeded());

        record_spend(root.path(), &id, 6.0).unwrap();
        let cred = get_by_id(root.path(), &id).unwrap().unwrap();
        assert!(!cred.is_budget_exceeded());

        record_spend(root.path(), &id, 5.0).unwrap();
        let cred = get_by_id(root.path(), &id).unwrap().unwrap();
        assert!(cred.is_budget_exceeded());
    }

    #[test]
    fn save_with_options_rejects_empty_control_chars_and_invalid_vendor_env() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        assert!(save_with_options(root.path(), "OPENAI_API_KEY", "", None, None).is_err());
        assert!(save_with_options(root.path(), "OPENAI_API_KEY", "sk-\n-bad", None, None).is_err());
        assert!(save_with_options(root.path(), "", "sk-valid-key", None, None).is_err());
        assert!(save_with_options(root.path(), "BAD-ENV", "sk-valid-key", None, None).is_err());
    }

    #[test]
    fn mask_key_handles_short_keys_resave_preserves_spend_and_remove_by_id_works() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        assert!(list_all(root.path()).unwrap().is_empty());

        let short_id =
            save_with_options(root.path(), "OPENAI_API_KEY", "short123", Some(20.0), None).unwrap();
        let short_cred = get_by_id(root.path(), &short_id).unwrap().unwrap();
        assert_eq!(short_cred.mask_key(), "***");
        assert!(record_spend(root.path(), &short_id, -1.0).is_err());

        record_spend(root.path(), &short_id, 7.5).unwrap();
        let resave_id = save_with_options(
            root.path(),
            "OPENAI_API_KEY",
            "short123",
            Some(50.0),
            Some("updated-cap".into()),
        )
        .unwrap();
        assert_eq!(short_id, resave_id);
        let updated = get_by_id(root.path(), &short_id).unwrap().unwrap();
        assert_eq!(updated.current_spend_usd, Some(7.5));
        assert_eq!(updated.max_budget_usd, Some(50.0));

        remove_by_id(root.path(), &short_id).unwrap();
        assert!(get_by_id(root.path(), &short_id).unwrap().is_none());
        assert!(list_all(root.path()).unwrap().is_empty());
    }

    #[test]
    fn enrolled_vault_seals_cloud_credentials_at_rest() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        susi_config::vault_store::enroll_master_key(root.path(), Some("keyfile"), None).unwrap();
        let cred_dir = root.path().join("cloud-credentials");
        let id = save(&cred_dir, "OPENAI_API_KEY", "sk-sealed-at-rest-42").unwrap();
        let cred = get_by_id(&cred_dir, &id).unwrap().unwrap();
        assert_eq!(cred.encrypted, Some(true));
        assert_ne!(cred.key(), "sk-sealed-at-rest-42");
        assert_eq!(cred.mask_key(), "[sealed]");
        assert_eq!(cred.unseal_key(&cred_dir).unwrap(), "sk-sealed-at-rest-42");
    }
}
