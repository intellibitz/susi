//! OS keyring-backed cloud key storage with encrypted-file fallback.
//!
//! Production uses Secret Service / Keychain / Credential Manager via a
//! pluggable [`KeyringBackend`]. Tests inject [`FakeKeyring`]. Plaintext
//! `cloud.env` entries migrate into the backend; env-var overrides still win.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const SERVICE: &str = "susi-cloud-keys";

/// Pluggable keyring / encrypted-file store.
pub trait KeyringBackend {
    fn set(&mut self, account: &str, secret: &str) -> Result<(), String>;
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn delete(&mut self, account: &str) -> Result<(), String>;
}

/// In-memory fake for hermetic tests.
#[derive(Debug, Default, Clone)]
pub struct FakeKeyring {
    entries: BTreeMap<String, String>,
}

impl KeyringBackend for FakeKeyring {
    fn set(&mut self, account: &str, secret: &str) -> Result<(), String> {
        self.entries.insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.entries.get(account).cloned())
    }

    fn delete(&mut self, account: &str) -> Result<(), String> {
        self.entries.remove(account);
        Ok(())
    }
}

/// Encrypted-file fallback: XOR with a host-local key material (not for
/// high-assurance crypto — keeps secrets off plaintext disk when keyring
/// is unavailable). Real OS keyring is preferred when present.
#[derive(Debug, Default)]
pub struct EncryptedFileFallback {
    /// account -> base64-ish obfuscated payload (hex of XOR'd bytes).
    map: BTreeMap<String, String>,
    key: u8,
}

impl EncryptedFileFallback {
    #[must_use]
    pub fn with_key(key: u8) -> Self {
        Self {
            map: BTreeMap::new(),
            key: key.max(1),
        }
    }

    fn encode(&self, secret: &str) -> String {
        secret
            .bytes()
            .map(|b| format!("{:02x}", b ^ self.key))
            .collect()
    }

    fn decode(&self, hex: &str) -> Result<String, String> {
        if !hex.len().is_multiple_of(2) {
            return Err("bad cipher".into());
        }
        let mut out = Vec::with_capacity(hex.len() / 2);
        let bytes = hex.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let h = std::str::from_utf8(&bytes[i..i + 2]).map_err(|e| e.to_string())?;
            let v = u8::from_str_radix(h, 16).map_err(|e| e.to_string())?;
            out.push(v ^ self.key);
            i += 2;
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }
}

impl KeyringBackend for EncryptedFileFallback {
    fn set(&mut self, account: &str, secret: &str) -> Result<(), String> {
        self.map.insert(account.to_string(), self.encode(secret));
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match self.map.get(account) {
            Some(h) => Ok(Some(self.decode(h)?)),
            None => Ok(None),
        }
    }

    fn delete(&mut self, account: &str) -> Result<(), String> {
        self.map.remove(account);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudKeyEntry {
    pub provider: String,
    pub value: String,
}

/// Store that prefers env overrides, then keyring, migrating plaintext files.
pub struct KeyringStorage<B: KeyringBackend> {
    backend: B,
    /// account -> env override value (e.g. OPENAI_API_KEY).
    env_overrides: BTreeMap<String, String>,
}

impl<B: KeyringBackend> KeyringStorage<B> {
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            env_overrides: BTreeMap::new(),
        }
    }

    pub fn set_env_override(&mut self, account: &str, value: &str) {
        self.env_overrides
            .insert(account.to_string(), value.to_string());
    }

    pub fn put(&mut self, provider: &str, secret: &str) -> Result<(), String> {
        self.backend.set(&account(provider), secret)
    }

    pub fn get(&self, provider: &str) -> Result<Option<String>, String> {
        let acct = account(provider);
        if let Some(v) = self.env_overrides.get(&acct) {
            return Ok(Some(v.clone()));
        }
        self.backend.get(&acct)
    }

    /// Migrate `KEY=value` lines from a plaintext cloud.env into the backend.
    pub fn migrate_cloud_env(&mut self, contents: &str) -> Result<usize, String> {
        let mut n = 0;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let provider = k.trim().trim_end_matches("_API_KEY").to_ascii_lowercase();
            self.put(&provider, v.trim())?;
            n += 1;
        }
        Ok(n)
    }

    pub fn service_name() -> &'static str {
        SERVICE
    }

    /// Persist fallback map (tests / offline).
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}

fn account(provider: &str) -> String {
    format!("{SERVICE}/{provider}")
}

/// Load cloud.env from disk if present (caller supplies path for hermeticity).
pub fn read_cloud_env(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_file_fallback_and_fake_keyring_roundtrip_and_bad_cipher_rejection() {
        let mut fake = FakeKeyring::default();
        assert_eq!(fake.get("susi-cloud-keys/openai").unwrap(), None);
        fake.set("susi-cloud-keys/openai", "sk-fake-123").unwrap();
        assert_eq!(
            fake.get("susi-cloud-keys/openai").unwrap(),
            Some("sk-fake-123".to_string())
        );
        fake.delete("susi-cloud-keys/openai").unwrap();
        assert_eq!(fake.get("susi-cloud-keys/openai").unwrap(), None);

        let mut enc = EncryptedFileFallback::with_key(0x5A);
        enc.set("susi-cloud-keys/openai", "sk-secret-value")
            .unwrap();
        let stored_hex = enc
            .map
            .get("susi-cloud-keys/openai")
            .expect("stored cipher entry");
        assert_ne!(stored_hex, "sk-secret-value");
        assert!(!stored_hex.contains("sk-secret"));
        assert_eq!(
            enc.get("susi-cloud-keys/openai").unwrap(),
            Some("sk-secret-value".to_string())
        );
        enc.delete("susi-cloud-keys/openai").unwrap();
        assert_eq!(enc.get("susi-cloud-keys/openai").unwrap(), None);

        let mut clamped = EncryptedFileFallback::with_key(0);
        assert_eq!(clamped.key, 1);
        clamped
            .set("susi-cloud-keys/anthropic", "sk-ant-999")
            .unwrap();
        assert_ne!(
            clamped.map.get("susi-cloud-keys/anthropic").unwrap(),
            "sk-ant-999"
        );
        assert_eq!(
            clamped.get("susi-cloud-keys/anthropic").unwrap(),
            Some("sk-ant-999".to_string())
        );

        assert_eq!(enc.decode("abc"), Err("bad cipher".to_string()));
        assert!(enc.decode("zz").is_err());
    }

    #[test]
    fn keyring_storage_env_override_precedence_and_cloud_env_migration() {
        assert_eq!(
            KeyringStorage::<FakeKeyring>::service_name(),
            "susi-cloud-keys"
        );

        let mut store = KeyringStorage::new(FakeKeyring::default());
        assert_eq!(store.get("openai").unwrap(), None);

        store.put("openai", "backend_val").unwrap();
        assert_eq!(
            store.get("openai").unwrap(),
            Some("backend_val".to_string())
        );

        store.set_env_override("susi-cloud-keys/openai", "override_val");
        assert_eq!(
            store.get("openai").unwrap(),
            Some("override_val".to_string())
        );
        assert_eq!(
            store.backend_mut().get("susi-cloud-keys/openai").unwrap(),
            Some("backend_val".to_string())
        );

        let env_contents = "OPENAI_API_KEY=sk-1\n# comment\nANTHROPIC_API_KEY=sk-2\n";
        let mut mig_store = KeyringStorage::new(FakeKeyring::default());
        let migrated = mig_store.migrate_cloud_env(env_contents).unwrap();
        assert_eq!(migrated, 2);
        assert_eq!(mig_store.get("openai").unwrap(), Some("sk-1".to_string()));
        assert_eq!(
            mig_store.get("anthropic").unwrap(),
            Some("sk-2".to_string())
        );

        let dir =
            std::env::temp_dir().join(format!("susi-keyring-storage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let env_path = dir.join("cloud.env");
        std::fs::write(&env_path, env_contents).expect("write cloud.env");
        let loaded_text = read_cloud_env(&env_path).expect("read cloud.env");
        assert_eq!(loaded_text, env_contents);
        assert!(read_cloud_env(&dir.join("missing.env")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
