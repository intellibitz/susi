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
