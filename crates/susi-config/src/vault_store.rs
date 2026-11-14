//! Cross-OS unattended encrypted secret store (`SealedVault`) and host keystore
//! unsealing (`SUSI_VAULT_KEY`, `CREDENTIALS_DIRECTORY`, Linux `systemd-creds`,
//! Freedesktop `secret-tool`, Windows DPAPI, or `0600` enrolled host keyfile).
//!
//! Format on disk (`~/.susi/vault.sealed.json`):
//! - Encrypted with RFC 8439 ChaCha20-Poly1305 AEAD (`susi-vendor-chacha20poly1305`)
//! - 32-byte per-vault random salt + fresh 12-byte random nonce on every write
//! - Subkey derived via domain-separated SHA-256 over `(master_key, salt)`
//! - AAD binds envelope format, KDF tag, and salt so ciphertext cannot be transplanted
//! - Zero secret values are ever printed, logged, or stored in `vault.json` metadata.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{compiler_fence, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

const KEYSTORE_TIMEOUT: Duration = Duration::from_secs(15);

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::cloud_env::parse_env_file;
use super::json_util::{atomic_write_bytes, create_private_dir, file_node};

/// Non-secret vault metadata filename inside the Susi config directory.
pub const VAULT_CONFIG_FILE: &str = "vault.json";
/// AEAD-sealed vault filename inside the Susi config directory.
pub const SEALED_VAULT_FILE: &str = "vault.sealed.json";
/// Wire format identifier for the shared core/commercial sealed vault.
pub const SEALED_VAULT_FORMAT: &str = "susi-sealed-vault-v1";
/// Wire KDF identifier.
pub const SEALED_VAULT_KDF: &str = "sha256-domain-v1";
/// Inline prefix used when sealing individual `cloud-credentials/*.json` values.
pub const SEALED_VALUE_PREFIX: &str = "enc:v1:";

/// Cloud / container / CI explicit master key environment variable.
pub const VAULT_KEY_ENV: &str = "SUSI_VAULT_KEY";
/// File path containing the vault master key (e.g. mounted Docker/K8s secret).
pub const VAULT_KEY_FILE_ENV: &str = "SUSI_VAULT_KEY_FILE";
/// Alternate explicit vault passphrase environment variable.
pub const VAULT_PASSPHRASE_ENV: &str = "SUSI_VAULT_PASSPHRASE";
/// File path containing the vault passphrase.
pub const VAULT_PASSPHRASE_FILE_ENV: &str = "SUSI_VAULT_PASSPHRASE_FILE";
/// Explicit Susi credentials directory override (checked alongside `CREDENTIALS_DIRECTORY`).
pub const SUSI_CREDENTIALS_DIR_ENV: &str = "SUSI_CREDENTIALS_DIRECTORY";
/// Explicit non-interactive KeePassXC `.kdbx` passphrase environment variable.
pub const KDBX_PASSPHRASE_ENV: &str = "SUSI_VAULT_KDBX_PASSPHRASE";
/// File containing the KeePassXC `.kdbx` passphrase.
pub const KDBX_PASSPHRASE_FILE_ENV: &str = "SUSI_VAULT_KDBX_PASSPHRASE_FILE";

static VAULT_CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);
static CLOUD_ENV_CACHE: OnceLock<Mutex<BTreeMap<PathBuf, CachedCloudEnv>>> = OnceLock::new();
static UNLOCK_WARN_ONCE: OnceLock<Mutex<BTreeMap<PathBuf, String>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
struct VaultCacheStamp {
    generation: u64,
    file_sig: Option<String>,
    unlock_digest: [u8; 32],
    env_digest: [u8; 32],
}

#[derive(Debug, Clone)]
struct CachedCloudEnv {
    stamp: VaultCacheStamp,
    entries: Vec<(String, String)>,
}

fn bump_vault_cache_generation() {
    VAULT_CACHE_GENERATION.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn write_private_vault_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    atomic_write_bytes(path, bytes)?;
    #[cfg(windows)]
    {
        let _ = winapi::acl::restrict_to_current_user(path);
    }
    bump_vault_cache_generation();
    Ok(())
}

/// Purpose identifier for the local AEAD vault master key.
pub const PURPOSE_VAULT: &str = "vault";
/// Purpose identifier for the cross-OS KeePassXC `.kdbx` unlock passphrase.
pub const PURPOSE_KDBX: &str = "kdbx";

const MASTER_DOMAIN: &[u8] = b"susi-vault-passphrase-v1";
const FP_DOMAIN: &[u8] = b"susi-vault-fp-v1";
const AEAD_KEY_DOMAIN: &[u8] = b"susi-vault-aead-v1";
const CRED_KEY_DOMAIN: &[u8] = b"susi-cred-aead-v1";
const CRED_AAD: &[u8] = b"susi-cred-v1";

/// Overwrite byte slice with zeros and emit a compiler fence to prevent dead-store elimination.
pub fn wipe_bytes(bytes: &mut [u8]) {
    bytes.fill(0);
    compiler_fence(Ordering::SeqCst);
}

/// Overwrite a string buffer cleanly and clear its length.
pub fn wipe_string(secret: &mut String) {
    let owned = std::mem::take(secret);
    drop_secret_string(owned);
}

/// Best-effort in-place wipe of an owned String's backing buffer using `into_bytes` + `wipe_bytes`.
pub fn drop_secret_string(secret: String) {
    let mut raw = secret.into_bytes();
    let cap = raw.capacity();
    if cap > raw.len() {
        raw.resize(cap, 0);
    }
    wipe_bytes(&mut raw);
}

/// How an unattended host obtains its unlock material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockBackendKind {
    EnvBootstrap,
    CredentialsDirectory,
    SystemdCreds,
    MacosKeychain,
    WindowsDpapi,
    SecretService,
    PrivateKeyFile,
}

impl UnlockBackendKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvBootstrap => "env",
            Self::CredentialsDirectory => "credentials-directory",
            Self::SystemdCreds => "systemd-creds",
            Self::MacosKeychain => "macos-keychain",
            Self::WindowsDpapi => "dpapi",
            Self::SecretService => "secret-tool",
            Self::PrivateKeyFile => "keyfile",
        }
    }

    /// Parse a user-supplied backend flag.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "systemd" | "systemd-creds" => Ok(Self::SystemdCreds),
            "keychain" | "macos-keychain" | "macos_keychain" | "osx-keychain" => {
                Ok(Self::MacosKeychain)
            }
            "dpapi" | "windows-dpapi" => Ok(Self::WindowsDpapi),
            "secret-tool" | "secret-service" | "freedesktop" => Ok(Self::SecretService),
            "keyfile" | "file" => Ok(Self::PrivateKeyFile),
            other => Err(format!(
                "unsupported unlock backend `{other}` (expected auto, systemd-creds, macos-keychain, dpapi, secret-tool, or keyfile)"
            )),
        }
    }
}

impl fmt::Display for UnlockBackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Non-secret vault & Google Drive sync metadata (`~/.susi/vault.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultConfig {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_db_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdrive_sync_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rclone_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepassxc_cli: Option<String>,
    #[serde(default)]
    pub unlock_backends: Vec<String>,
    #[serde(default)]
    pub kdbx_unlock_backends: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_unix: Option<u64>,
    #[serde(default)]
    pub synced_entry_count: usize,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            version: 1,
            vault_db_path: None,
            key_file_path: None,
            gdrive_sync_path: None,
            rclone_remote: None,
            keepassxc_cli: None,
            unlock_backends: Vec::new(),
            kdbx_unlock_backends: Vec::new(),
            last_sync_unix: None,
            synced_entry_count: 0,
        }
    }
}

impl VaultConfig {
    /// Path to `vault.json` under `dir`.
    #[must_use]
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(VAULT_CONFIG_FILE)
    }

    /// Load `vault.json` if present and valid; return default otherwise.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let path = Self::path_in(dir);
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Persist `vault.json` atomically with `0600` permissions.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        create_private_dir(dir).map_err(|e| format!("create config dir: {e}"))?;
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize vault.json: {e}"))?;
        write_private_vault_bytes(&Self::path_in(dir), format!("{text}\n").as_bytes())
            .map_err(|e| format!("write vault.json: {e}"))
    }

    fn record_backend(&mut self, purpose: &str, kind: UnlockBackendKind) {
        let target = if purpose == PURPOSE_KDBX {
            &mut self.kdbx_unlock_backends
        } else {
            &mut self.unlock_backends
        };
        let label = kind.as_str().to_string();
        if !target.iter().any(|existing| existing == &label) {
            target.push(label);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedVaultEnvelope {
    format: String,
    kdf: String,
    key_fingerprint: String,
    salt_hex: String,
    nonce_hex: String,
    ciphertext_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SealedPayload {
    entries: Vec<SealedEntryWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedEntryWire {
    scope: String,
    name: String,
    value: String,
}

/// Decrypted in-memory view of `~/.susi/vault.sealed.json`. Wiped on drop.
#[derive(Debug, Clone)]
pub struct SealedVault {
    salt: [u8; 32],
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

impl Default for SealedVault {
    fn default() -> Self {
        let mut salt = [0u8; 32];
        let _ = getrandom::fill(&mut salt);
        Self {
            salt,
            entries: BTreeMap::new(),
        }
    }
}

impl Drop for SealedVault {
    fn drop(&mut self) {
        wipe_bytes(&mut self.salt);
        let drained = std::mem::take(&mut self.entries);
        for (scope, inner) in drained {
            drop_secret_string(scope);
            for (name, val) in inner {
                drop_secret_string(name);
                drop_secret_string(val);
            }
        }
    }
}

impl SealedVault {
    /// Path to `vault.sealed.json` inside `dir`.
    #[must_use]
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(SEALED_VAULT_FILE)
    }

    /// Open and authenticate `vault.sealed.json` from `dir` using unattended host unlock.
    /// Returns `Ok(None)` when no `vault.sealed.json` file exists yet.
    pub fn open(dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path_in(dir);
        if !path.is_file() {
            return Ok(None);
        }
        let mut master = resolve_master_key(dir)?
            .ok_or_else(|| "vault.sealed.json exists but no unattended unlock key is enrolled or set in SUSI_VAULT_KEY".to_string())?;
        let res = Self::open_with_key(&path, &master);
        wipe_bytes(&mut master);
        res
    }

    /// Open `path` with an explicit 256-bit `master_key`.
    pub fn open_with_key(path: &Path, master_key: &[u8; 32]) -> Result<Option<Self>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let raw =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let env: SealedVaultEnvelope =
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if env.format != SEALED_VAULT_FORMAT {
            return Err(format!("unsupported sealed vault format `{}`", env.format));
        }
        if env.kdf != SEALED_VAULT_KDF {
            return Err(format!("unsupported sealed vault KDF `{}`", env.kdf));
        }
        let expected_fp = key_fingerprint(master_key);
        if !constant_time_eq_bytes(env.key_fingerprint.as_bytes(), expected_fp.as_bytes()) {
            return Err(format!(
                "sealed vault key fingerprint mismatch (expected `{expected_fp}`, found `{}`)",
                env.key_fingerprint
            ));
        }
        let salt_vec = susi_paths::hex::decode(env.salt_hex.trim())
            .map_err(|e| format!("invalid vault salt hex: {e}"))?;
        let salt: [u8; 32] = salt_vec
            .as_slice()
            .try_into()
            .map_err(|_| "vault salt must be 32 bytes".to_string())?;
        let nonce_vec = susi_paths::hex::decode(env.nonce_hex.trim())
            .map_err(|e| format!("invalid vault nonce hex: {e}"))?;
        if nonce_vec.len() != 12 {
            return Err("vault nonce must be 12 bytes".to_string());
        }
        let ct = susi_paths::hex::decode(env.ciphertext_hex.trim())
            .map_err(|e| format!("invalid vault ciphertext hex: {e}"))?;
        let mut subkey = derive_vault_subkey(master_key, &salt);
        let cipher = ChaCha20Poly1305::new(Key::from(&subkey));
        wipe_bytes(&mut subkey);
        let aad = build_vault_aad(&salt);
        let mut pt = cipher
            .decrypt(
                Nonce::from_slice(&nonce_vec),
                Payload {
                    msg: &ct,
                    aad: &aad,
                },
            )
            .map_err(|_| {
                "vault authentication failed (wrong key or tampered ciphertext)".to_string()
            })?;
        let payload_str = std::str::from_utf8(&pt)
            .map_err(|_| "decrypted vault payload is not valid UTF-8".to_string())?;
        let parsed: Result<SealedPayload, _> = serde_json::from_str(payload_str);
        wipe_bytes(&mut pt);
        let payload = parsed.map_err(|e| format!("parse vault payload: {e}"))?;
        let mut entries: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for item in payload.entries {
            if !item.scope.trim().is_empty() && !item.name.trim().is_empty() {
                entries
                    .entry(item.scope)
                    .or_default()
                    .insert(item.name, item.value);
            }
        }
        Ok(Some(Self { salt, entries }))
    }

    /// Open an existing sealed vault in `dir` or construct a fresh empty vault when absent.
    pub fn open_or_init(dir: &Path) -> Result<Self, String> {
        match Self::open(dir)? {
            Some(v) => Ok(v),
            None => Ok(Self::default()),
        }
    }

    /// Encrypt and persist atomically (`0600`) to `dir/vault.sealed.json` using unattended host unlock.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let mut master = resolve_master_key(dir)?.ok_or_else(|| {
            "cannot save vault.sealed.json without an enrolled unlock key or SUSI_VAULT_KEY"
                .to_string()
        })?;
        let res = self.save_with_key(&Self::path_in(dir), &master);
        wipe_bytes(&mut master);
        res
    }

    /// Encrypt and persist atomically (`0600`) to `path` with explicit `master_key`.
    pub fn save_with_key(&self, path: &Path, master_key: &[u8; 32]) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent).map_err(|e| format!("create vault dir: {e}"))?;
        }
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| format!("generate vault nonce: {e}"))?;

        let mut payload_entries = Vec::new();
        for (scope, map) in &self.entries {
            for (name, value) in map {
                payload_entries.push(SealedEntryWire {
                    scope: scope.clone(),
                    name: name.clone(),
                    value: value.clone(),
                });
            }
        }
        let payload_json = serde_json::to_string(&SealedPayload {
            entries: payload_entries,
        })
        .map_err(|e| format!("serialize vault payload: {e}"))?;
        let mut pt = payload_json.into_bytes();

        let mut subkey = derive_vault_subkey(master_key, &self.salt);
        let cipher = ChaCha20Poly1305::new(Key::from(&subkey));
        wipe_bytes(&mut subkey);
        let aad = build_vault_aad(&self.salt);
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &pt,
                    aad: &aad,
                },
            )
            .map_err(|_| "vault AEAD encryption failed".to_string());
        wipe_bytes(&mut pt);
        let ct = ct?;

        let envelope = SealedVaultEnvelope {
            format: SEALED_VAULT_FORMAT.to_string(),
            kdf: SEALED_VAULT_KDF.to_string(),
            key_fingerprint: key_fingerprint(master_key),
            salt_hex: susi_paths::hex::encode(self.salt),
            nonce_hex: susi_paths::hex::encode(nonce),
            ciphertext_hex: susi_paths::hex::encode(&ct),
        };
        let mut encoded = serde_json::to_string_pretty(&envelope)
            .map_err(|e| format!("encode sealed vault envelope: {e}"))?;
        encoded.push('\n');
        write_private_vault_bytes(path, encoded.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// Read a secret value for `(scope, name)`.
    #[must_use]
    pub fn get(&self, scope: &str, name: &str) -> Option<&str> {
        self.entries
            .get(scope)
            .and_then(|map| map.get(name))
            .map(String::as_str)
    }

    /// Look up a named key by exact `(scope, name)` or fall back across standard scopes.
    #[must_use]
    pub fn lookup_any(&self, key_or_path: &str) -> Option<&str> {
        let trimmed = key_or_path.trim();
        if let Some((scope, name)) = trimmed.split_once('/') {
            if let Some(v) = self.get(scope.trim(), name.trim()) {
                return Some(v);
            }
        }
        for scope in ["cloud", "commercial", "env", "agent", "kdbx"] {
            if let Some(v) = self.get(scope, trimmed) {
                return Some(v);
            }
        }
        None
    }

    /// Insert or update `(scope, name)`.
    pub fn set(&mut self, scope: &str, name: &str, value: &str) {
        self.entries
            .entry(scope.trim().to_string())
            .or_default()
            .insert(name.trim().to_string(), value.to_string());
    }

    /// Remove `(scope, name)`. Returns `true` if a value was present.
    pub fn remove(&mut self, scope: &str, name: &str) -> bool {
        self.entries
            .get_mut(scope)
            .is_some_and(|map| map.remove(name).is_some())
    }

    /// Return `(name, value)` pairs inside `scope`.
    #[must_use]
    pub fn scope_entries(&self, scope: &str) -> Vec<(String, String)> {
        self.entries
            .get(scope)
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// Return all `(scope, name)` metadata keys WITHOUT secret values.
    #[must_use]
    pub fn list_keys(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (scope, map) in &self.entries {
            for name in map.keys() {
                out.push((scope.clone(), name.clone()));
            }
        }
        out
    }

    /// Total secret entries stored in the vault.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.values().map(BTreeMap::len).sum()
    }
}

/// Safe metadata snapshot of vault state (NEVER holds secret values).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStatus {
    pub config_dir: PathBuf,
    pub sealed_vault_exists: bool,
    pub unlocked: bool,
    pub unlock_backend: Option<String>,
    pub unlock_error: Option<String>,
    pub key_fingerprint: Option<String>,
    pub sealed_entry_count: usize,
    pub kdbx_unlock_enrolled: bool,
    pub kdbx_unlock_backend: Option<String>,
    pub vault_db_path: Option<PathBuf>,
    pub gdrive_sync_path: Option<PathBuf>,
    pub rclone_remote: Option<String>,
    pub last_sync_unix: Option<u64>,
    pub plaintext_cloud_env_keys: usize,
}

/// Outcome of migrating plaintext `cloud.env` into `SealedVault`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationSummary {
    pub migrated_keys: Vec<String>,
    pub cloud_env_cleared: bool,
}

/// Derive a stable 16-hex-char fingerprint of a 32-byte master key.
#[must_use]
pub fn key_fingerprint(master_key: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FP_DOMAIN);
    hasher.update(master_key);
    let digest = hasher.finalize();
    susi_paths::hex::encode(&digest[..8])
}

/// Normalize arbitrary passphrase or 64-hex-char key material into a 32-byte key.
#[must_use]
pub fn normalize_master_key(raw_secret: &str) -> [u8; 32] {
    let trimmed = raw_secret.trim();
    if trimmed.len() == 64 {
        if let Ok(decoded) = susi_paths::hex::decode(trimmed) {
            if let Ok(exact) = <[u8; 32]>::try_from(decoded.as_slice()) {
                return exact;
            }
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(MASTER_DOMAIN);
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn derive_vault_subkey(master_key: &[u8; 32], salt: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(AEAD_KEY_DOMAIN);
    hasher.update(master_key);
    hasher.update(salt);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn derive_cred_subkey(master_key: &[u8; 32], nonce: &[u8; 12]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CRED_KEY_DOMAIN);
    hasher.update(master_key);
    hasher.update(nonce);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn build_vault_aad(salt: &[u8; 32]) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(SEALED_VAULT_FORMAT.len() + 1 + SEALED_VAULT_KDF.len() + 1 + 32);
    aad.extend_from_slice(SEALED_VAULT_FORMAT.as_bytes());
    aad.push(0);
    aad.extend_from_slice(SEALED_VAULT_KDF.as_bytes());
    aad.push(0);
    aad.extend_from_slice(salt);
    aad
}

fn constant_time_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Resolve the 32-byte AEAD master key for `dir` without prompting for a password.
pub fn resolve_master_key(dir: &Path) -> Result<Option<[u8; 32]>, String> {
    let Some((mut raw, _backend)) = resolve_unlock_secret(dir, PURPOSE_VAULT)? else {
        return Ok(None);
    };
    let key = normalize_master_key(&raw);
    wipe_string(&mut raw);
    Ok(Some(key))
}

fn record_unlock_err(slot: &mut Option<String>, err: String) {
    if slot.is_none() {
        *slot = Some(err);
    }
}

/// Resolve unattended unlock secret for `purpose` (`"vault"` or `"kdbx"`).
pub fn resolve_unlock_secret(
    dir: &Path,
    purpose: &str,
) -> Result<Option<(String, UnlockBackendKind)>, String> {
    if let Some(found) = resolve_from_env(purpose)? {
        return Ok(Some(found));
    }
    if let Some(found) = resolve_from_credentials_dir(purpose)? {
        return Ok(Some(found));
    }
    let mut host_err: Option<String> = None;
    #[cfg(target_os = "linux")]
    match unseal_systemd_cred(dir, purpose) {
        Ok(Some(secret)) => return Ok(Some((secret, UnlockBackendKind::SystemdCreds))),
        Ok(None) => {}
        Err(err) => record_unlock_err(&mut host_err, err),
    }
    #[cfg(target_os = "macos")]
    match unseal_macos_keychain(dir, purpose) {
        Ok(Some(secret)) => return Ok(Some((secret, UnlockBackendKind::MacosKeychain))),
        Ok(None) => {}
        Err(err) => record_unlock_err(&mut host_err, err),
    }
    #[cfg(windows)]
    match unseal_windows_dpapi(dir, purpose) {
        Ok(Some(secret)) => return Ok(Some((secret, UnlockBackendKind::WindowsDpapi))),
        Ok(None) => {}
        Err(err) => record_unlock_err(&mut host_err, err),
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    match unseal_secret_service(dir, purpose) {
        Ok(Some(secret)) => return Ok(Some((secret, UnlockBackendKind::SecretService))),
        Ok(None) => {}
        Err(err) => record_unlock_err(&mut host_err, err),
    }
    match read_private_keyfile(dir, purpose) {
        Ok(Some(secret)) => return Ok(Some((secret, UnlockBackendKind::PrivateKeyFile))),
        Ok(None) => {}
        Err(err) => record_unlock_err(&mut host_err, err),
    }
    if let Some(err) = host_err {
        return Err(err);
    }
    Ok(None)
}

fn resolve_from_env(purpose: &str) -> Result<Option<(String, UnlockBackendKind)>, String> {
    let (val_vars, file_vars): (&[&str], &[&str]) = if purpose == PURPOSE_KDBX {
        (
            &[KDBX_PASSPHRASE_ENV, "SUSI_KDBX_PASSPHRASE"],
            &[KDBX_PASSPHRASE_FILE_ENV, "SUSI_KDBX_PASSPHRASE_FILE"],
        )
    } else {
        (
            &[VAULT_KEY_ENV, VAULT_PASSPHRASE_ENV],
            &[VAULT_KEY_FILE_ENV, VAULT_PASSPHRASE_FILE_ENV],
        )
    };
    for name in val_vars {
        if let Ok(val) = std::env::var(name) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Ok(Some((trimmed.to_string(), UnlockBackendKind::EnvBootstrap)));
            }
        }
    }
    for name in file_vars {
        if let Ok(path_str) = std::env::var(name) {
            let trimmed_path = path_str.trim();
            if !trimmed_path.is_empty() {
                let mut raw = std::fs::read_to_string(trimmed_path)
                    .map_err(|e| format!("read {name} ({trimmed_path}): {e}"))?;
                let secret = raw.trim().to_string();
                wipe_string(&mut raw);
                if secret.is_empty() {
                    return Err(format!("{name} points to an empty file"));
                }
                return Ok(Some((secret, UnlockBackendKind::EnvBootstrap)));
            }
        }
    }
    Ok(None)
}

fn resolve_from_credentials_dir(
    purpose: &str,
) -> Result<Option<(String, UnlockBackendKind)>, String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["CREDENTIALS_DIRECTORY", SUSI_CREDENTIALS_DIR_ENV] {
        if let Ok(cred_dir) = std::env::var(var) {
            let trimmed = cred_dir.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
    }
    #[cfg(not(test))]
    {
        dirs.push(PathBuf::from("/run/secrets"));
        dirs.push(PathBuf::from("/var/run/secrets/susi"));
    }
    resolve_from_secret_dirs(purpose, &dirs)
}

fn resolve_from_secret_dirs(
    purpose: &str,
    dirs: &[PathBuf],
) -> Result<Option<(String, UnlockBackendKind)>, String> {
    let names: &[&str] = if purpose == PURPOSE_KDBX {
        &["susi-kdbx-passphrase", "kdbx.passphrase"]
    } else {
        &["susi-vault-key", "vault.key", "susi-vault-passphrase"]
    };
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        for file in names {
            let candidate = dir.join(file);
            if candidate.is_file() {
                let mut raw = std::fs::read_to_string(&candidate)
                    .map_err(|e| format!("read {}/{file}: {e}", dir.display()))?;
                let secret = raw.trim().to_string();
                wipe_string(&mut raw);
                if !secret.is_empty() {
                    return Ok(Some((secret, UnlockBackendKind::CredentialsDirectory)));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn cred_file_path(dir: &Path, purpose: &str) -> PathBuf {
    dir.join(format!("{purpose}.unlock.cred"))
}

#[cfg(windows)]
fn dpapi_file_path(dir: &Path, purpose: &str) -> PathBuf {
    dir.join(format!("{purpose}.unlock.dpapi"))
}

#[cfg(any(target_os = "macos", test))]
fn macos_keychain_marker_path(dir: &Path, purpose: &str) -> PathBuf {
    dir.join(format!("{purpose}.unlock.keychain"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn secret_service_marker_path(dir: &Path, purpose: &str) -> PathBuf {
    dir.join(format!("{purpose}.unlock.secretservice"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn secret_service_legacy_marker_path(dir: &Path, purpose: &str) -> PathBuf {
    dir.join(format!("{purpose}.unlock.secret"))
}

fn keyfile_path(dir: &Path, purpose: &str) -> PathBuf {
    dir.join(format!("{purpose}.unlock.key"))
}

fn encode_secret_payload(purpose: &str, secret: &str) -> String {
    if purpose == PURPOSE_VAULT && !secret.contains(['\r', '\n']) {
        format!("{}\n", secret.trim())
    } else {
        format!("hex:{}\n", susi_paths::hex::encode(secret.as_bytes()))
    }
}

fn decode_secret_payload(raw: &str, label: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is empty"));
    }
    if let Some(hex_body) = trimmed.strip_prefix("hex:") {
        let mut bytes =
            susi_paths::hex::decode(hex_body).map_err(|e| format!("decode {label}: {e}"))?;
        let text =
            String::from_utf8(bytes.clone()).map_err(|_| format!("{label} is not valid UTF-8"));
        wipe_bytes(&mut bytes);
        return text;
    }
    Ok(trimmed.to_string())
}

#[cfg(target_os = "linux")]
fn seal_systemd_cred(dir: &Path, purpose: &str, secret: &str) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| format!("create config dir: {e}"))?;
    let dest = cred_file_path(dir, purpose);
    let cred_name = format!("susi-{purpose}");

    // Try TPM2+host first, then host-only, then systemd default key mode via stdout pipe
    // so no temporary plaintext or unencrypted credential file ever hits disk.
    for key_flag in [Some("--with-key=host+tpm2"), Some("--with-key=host"), None] {
        let mut cmd = Command::new("systemd-creds");
        cmd.args(["encrypt", "--user"]);
        if let Some(flag) = key_flag {
            cmd.arg(flag);
        }
        cmd.arg(format!("--name={cred_name}")).arg("-").arg("-");
        let out = susi_paths::console::run_output_within(
            &mut cmd,
            Some(secret.as_bytes()),
            KEYSTORE_TIMEOUT,
        )
        .map_err(|e| format!("systemd-creds: {e}"))?;
        if out.status.success() && !out.stdout.is_empty() {
            write_private_vault_bytes(&dest, &out.stdout)
                .map_err(|e| format!("write {}: {e}", dest.display()))?;
            return Ok(());
        }
    }
    Err("systemd-creds --user encrypt failed on this host".to_string())
}

#[cfg(target_os = "linux")]
fn unseal_systemd_cred(dir: &Path, purpose: &str) -> Result<Option<String>, String> {
    let src = cred_file_path(dir, purpose);
    if !src.is_file() {
        return Ok(None);
    }
    let cred_name = format!("susi-{purpose}");
    let mut cmd = Command::new("systemd-creds");
    cmd.args(["decrypt", "--user", &format!("--name={cred_name}")])
        .arg(&src)
        .arg("-");
    let out = susi_paths::console::run_output_within(&mut cmd, None, KEYSTORE_TIMEOUT)
        .map_err(|e| format!("run systemd-creds decrypt: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "systemd-creds decrypt {} failed: {err}",
            src.display()
        ));
    }
    let secret = String::from_utf8(out.stdout)
        .map_err(|_| "systemd-creds returned non-UTF-8 secret".to_string())?
        .trim()
        .to_string();
    if secret.is_empty() {
        return Err("systemd-creds returned an empty secret".to_string());
    }
    Ok(Some(secret))
}

#[cfg(any(windows, test))]
pub(crate) fn windows_powershell_exe_from(system_root: Option<&str>) -> String {
    match system_root.map(str::trim).filter(|s| !s.is_empty()) {
        Some(root) => format!(
            "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            root.trim_end_matches(['\\', '/'])
        ),
        None => "powershell.exe".to_string(),
    }
}

#[cfg(windows)]
fn windows_powershell_exe() -> String {
    let root = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("WINDIR"))
        .ok();
    windows_powershell_exe_from(root.as_deref())
}

#[cfg(windows)]
fn seal_windows_dpapi(dir: &Path, purpose: &str, secret: &str) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| format!("create config dir: {e}"))?;
    let dest = dpapi_file_path(dir, purpose);
    let mut hex_secret = susi_paths::hex::encode(secret.as_bytes());
    let script = concat!(
        "$ErrorActionPreference='Stop';",
        "Add-Type -AssemblyName System.Security;",
        "$raw=[Console]::In.ReadToEnd().Trim();",
        "$bytes=[Text.Encoding]::UTF8.GetBytes($raw);",
        "$prot=[System.Security.Cryptography.ProtectedData]::Protect($bytes,$null,'CurrentUser');",
        "[Convert]::ToBase64String($prot)"
    );
    let mut cmd = Command::new(windows_powershell_exe());
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    let out = susi_paths::console::run_output_within(
        &mut cmd,
        Some(hex_secret.as_bytes()),
        KEYSTORE_TIMEOUT,
    );
    wipe_string(&mut hex_secret);
    let out = out.map_err(|e| format!("powershell for DPAPI: {e}"))?;
    if !out.status.success() {
        return Err("Windows DPAPI Protect failed".to_string());
    }
    let b64 = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b64.is_empty() {
        return Err("Windows DPAPI Protect returned empty ciphertext".to_string());
    }
    write_private_vault_bytes(&dest, format!("{b64}\n").as_bytes())
        .map_err(|e| format!("write {}: {e}", dest.display()))
}

#[cfg(windows)]
fn unseal_windows_dpapi(dir: &Path, purpose: &str) -> Result<Option<String>, String> {
    let src = dpapi_file_path(dir, purpose);
    if !src.is_file() {
        return Ok(None);
    }
    let blob = std::fs::read_to_string(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let trimmed = blob.trim();
    if trimmed.is_empty() {
        return Err(format!("{} is empty", src.display()));
    }
    let script = concat!(
        "$ErrorActionPreference='Stop';",
        "Add-Type -AssemblyName System.Security;",
        "$b64=[Console]::In.ReadToEnd().Trim();",
        "$prot=[Convert]::FromBase64String($b64);",
        "$bytes=[System.Security.Cryptography.ProtectedData]::Unprotect($prot,$null,'CurrentUser');",
        "[Text.Encoding]::UTF8.GetString($bytes)"
    );
    let mut cmd = Command::new(windows_powershell_exe());
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    let out = susi_paths::console::run_output_within(
        &mut cmd,
        Some(trimmed.as_bytes()),
        KEYSTORE_TIMEOUT,
    )
    .map_err(|e| format!("powershell for DPAPI unseal: {e}"))?;
    if !out.status.success() {
        return Err(format!("Windows DPAPI Unprotect {} failed", src.display()));
    }
    let mut hex_secret = String::from_utf8(out.stdout)
        .map_err(|_| "DPAPI output is not UTF-8".to_string())?
        .trim()
        .to_string();
    let decoded_res = susi_paths::hex::decode(&hex_secret)
        .map_err(|_| "DPAPI output did not decode as expected secret encoding".to_string());
    wipe_string(&mut hex_secret);
    let mut decoded = decoded_res?;
    let secret =
        String::from_utf8(decoded.clone()).map_err(|_| "DPAPI secret is not UTF-8".to_string());
    wipe_bytes(&mut decoded);
    secret.map(Some)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn secret_service_scope_tag(dir: &Path, purpose: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(dir.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let tag = susi_paths::hex::encode(&digest[..6]);
    format!("susi-{purpose}-{tag}")
}

#[cfg(any(target_os = "macos", test))]
fn macos_keychain_add_script(account: &str, secret: &str) -> String {
    let mut encoded = format!("hex:{}", susi_paths::hex::encode(secret.as_bytes()));
    let script = format!(
        "add-generic-password -T /usr/bin/security -U -s susi-vault -a {account} -w {encoded}\n"
    );
    wipe_string(&mut encoded);
    script
}

#[cfg(target_os = "macos")]
fn seal_macos_keychain(dir: &Path, purpose: &str, secret: &str) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| format!("create config dir: {e}"))?;
    let marker = macos_keychain_marker_path(dir, purpose);
    let account = secret_service_scope_tag(dir, purpose);
    let mut script = macos_keychain_add_script(&account, secret);
    let mut cmd = Command::new("/usr/bin/security");
    cmd.arg("-i");
    let out =
        susi_paths::console::run_output_within(&mut cmd, Some(script.as_bytes()), KEYSTORE_TIMEOUT);
    wipe_string(&mut script);
    let out = out.map_err(|e| format!("macOS security keychain: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("macOS Keychain store failed: {stderr}"));
    }
    write_private_vault_bytes(&marker, format!("{account}\n").as_bytes())
        .map_err(|e| format!("write {}: {e}", marker.display()))
}

#[cfg(target_os = "macos")]
fn unseal_macos_keychain(dir: &Path, purpose: &str) -> Result<Option<String>, String> {
    let marker = macos_keychain_marker_path(dir, purpose);
    if !marker.is_file() {
        return Ok(None);
    }
    let account = std::fs::read_to_string(&marker)
        .map_err(|e| format!("read {}: {e}", marker.display()))?
        .trim()
        .to_string();
    if account.is_empty() {
        return Ok(None);
    }
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args([
        "find-generic-password",
        "-s",
        "susi-vault",
        "-a",
        &account,
        "-w",
    ]);
    let out = susi_paths::console::run_output_within(&mut cmd, None, KEYSTORE_TIMEOUT)
        .map_err(|e| format!("run macOS security keychain lookup: {e}"))?;
    if !out.status.success() {
        return Err("macOS Keychain lookup failed".to_string());
    }
    let mut raw = String::from_utf8(out.stdout)
        .map_err(|_| "macOS Keychain returned non-UTF-8 secret".to_string())?;
    let secret = decode_secret_payload(&raw, "macOS Keychain secret");
    wipe_string(&mut raw);
    secret.map(Some)
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn resolve_dbus_session_bus_with<F>(
    env_addr: Option<&str>,
    xdg_runtime_dir: Option<&str>,
    proc_status: Option<&str>,
    id_uid: Option<&str>,
    socket_exists: F,
) -> Option<String>
where
    F: Fn(&Path) -> bool,
{
    if let Some(addr) = env_addr.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(addr.to_string());
    }
    if let Some(runtime) = xdg_runtime_dir.map(str::trim).filter(|s| !s.is_empty()) {
        let trimmed = runtime.trim_end_matches('/');
        let bus = PathBuf::from(trimmed).join("bus");
        if socket_exists(&bus) {
            return Some(format!("unix:path={trimmed}/bus"));
        }
    }
    let uid = proc_status
        .and_then(|raw| {
            raw.lines().find_map(|line| {
                let rest = line.strip_prefix("Uid:")?;
                let val = rest.split_whitespace().next()?;
                (!val.is_empty() && val.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| val.to_string())
            })
        })
        .or_else(|| {
            id_uid
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .map(ToOwned::to_owned)
        });
    if let Some(uid) = uid {
        let bus_str = format!("/run/user/{uid}/bus");
        if socket_exists(Path::new(&bus_str)) {
            return Some(format!("unix:path={bus_str}"));
        }
    }
    None
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn resolve_dbus_session_bus() -> Option<String> {
    let env_addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
    let proc_status = std::fs::read_to_string("/proc/self/status").ok();
    let id_uid = if proc_status.is_none() {
        let mut cmd = Command::new("id");
        cmd.arg("-u");
        susi_paths::console::run_output_within(&mut cmd, None, KEYSTORE_TIMEOUT)
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
    } else {
        None
    };
    resolve_dbus_session_bus_with(
        env_addr.as_deref(),
        xdg_runtime.as_deref(),
        proc_status.as_deref(),
        id_uid.as_deref(),
        |p| p.exists(),
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn apply_dbus_session_env(cmd: &mut Command) {
    if let Some(bus) = resolve_dbus_session_bus() {
        cmd.env("DBUS_SESSION_BUS_ADDRESS", bus);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn seal_secret_service(dir: &Path, purpose: &str, secret: &str) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| format!("create config dir: {e}"))?;
    let marker = secret_service_marker_path(dir, purpose);
    let account = secret_service_scope_tag(dir, purpose);
    let label = format!("Susi Vault Unattended Key ({purpose})");
    let mut payload = encode_secret_payload(purpose, secret);
    let payload_trim = payload.trim_end_matches('\n').to_string();
    wipe_string(&mut payload);
    let mut cmd = Command::new("secret-tool");
    apply_dbus_session_env(&mut cmd);
    cmd.args([
        "store",
        &format!("--label={label}"),
        "service",
        "susi-vault",
        "account",
        &account,
    ]);
    let out = susi_paths::console::run_output_within(
        &mut cmd,
        Some(payload_trim.as_bytes()),
        KEYSTORE_TIMEOUT,
    );
    drop_secret_string(payload_trim);
    let out = out.map_err(|e| format!("secret-tool: {e}"))?;
    if !out.status.success() {
        return Err(
            "Freedesktop secret-tool store failed (session keyring locked or unavailable)"
                .to_string(),
        );
    }
    write_private_vault_bytes(&marker, format!("{account}\n").as_bytes())
        .map_err(|e| format!("write {}: {e}", marker.display()))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unseal_secret_service(dir: &Path, purpose: &str) -> Result<Option<String>, String> {
    let primary = secret_service_marker_path(dir, purpose);
    let legacy = secret_service_legacy_marker_path(dir, purpose);
    let marker = if primary.is_file() {
        primary
    } else if legacy.is_file() {
        legacy
    } else {
        return Ok(None);
    };
    let account = std::fs::read_to_string(&marker)
        .map_err(|e| format!("read {}: {e}", marker.display()))?
        .trim()
        .to_string();
    if account.is_empty() {
        return Ok(None);
    }
    let queries: [&[&str]; 2] = [
        &["lookup", "service", "susi-vault", "account", &account],
        &[
            "lookup",
            "service",
            "susi-vault",
            "purpose",
            purpose,
            "account",
            &account,
        ],
    ];
    for args in queries {
        let mut cmd = Command::new("secret-tool");
        apply_dbus_session_env(&mut cmd);
        cmd.args(args);
        let out = susi_paths::console::run_output_within(&mut cmd, None, KEYSTORE_TIMEOUT)
            .map_err(|e| format!("run secret-tool lookup: {e}"))?;
        if out.status.success() {
            let mut raw = String::from_utf8(out.stdout)
                .map_err(|_| "secret-tool returned non-UTF-8 secret".to_string())?;
            let secret = decode_secret_payload(&raw, "secret-tool secret");
            wipe_string(&mut raw);
            return secret.map(Some);
        }
    }
    Err("Freedesktop secret-tool lookup failed".to_string())
}

fn seal_private_keyfile(dir: &Path, purpose: &str, secret: &str) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| format!("create config dir: {e}"))?;
    let dest = keyfile_path(dir, purpose);
    let mut body = encode_secret_payload(purpose, secret);
    let res = write_private_vault_bytes(&dest, body.as_bytes())
        .map_err(|e| format!("write {}: {e}", dest.display()));
    wipe_string(&mut body);
    res
}

fn read_private_keyfile(dir: &Path, purpose: &str) -> Result<Option<String>, String> {
    let path = keyfile_path(dir, purpose);
    if !path.is_file() {
        return Ok(None);
    }
    let mut raw =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let label = path.display().to_string();
    let res = decode_secret_payload(&raw, &label);
    wipe_string(&mut raw);
    res.map(Some)
}

fn seal_secret_for_host(
    dir: &Path,
    purpose: &str,
    secret: &str,
    preferred_backend: Option<UnlockBackendKind>,
) -> Result<UnlockBackendKind, String> {
    match preferred_backend {
        Some(UnlockBackendKind::SystemdCreds) => {
            #[cfg(target_os = "linux")]
            {
                seal_systemd_cred(dir, purpose, secret)?;
                Ok(UnlockBackendKind::SystemdCreds)
            }
            #[cfg(not(target_os = "linux"))]
            Err("systemd-creds is only available on Linux".to_string())
        }
        Some(UnlockBackendKind::MacosKeychain) => {
            #[cfg(target_os = "macos")]
            {
                seal_macos_keychain(dir, purpose, secret)?;
                Ok(UnlockBackendKind::MacosKeychain)
            }
            #[cfg(not(target_os = "macos"))]
            Err("macos-keychain is only available on macOS".to_string())
        }
        Some(UnlockBackendKind::WindowsDpapi) => {
            #[cfg(windows)]
            {
                seal_windows_dpapi(dir, purpose, secret)?;
                Ok(UnlockBackendKind::WindowsDpapi)
            }
            #[cfg(not(windows))]
            Err("dpapi is only available on Windows".to_string())
        }
        Some(UnlockBackendKind::SecretService) => {
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                seal_secret_service(dir, purpose, secret)?;
                Ok(UnlockBackendKind::SecretService)
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            Err("secret-tool is only available on Unix/Linux Freedesktop hosts".to_string())
        }
        Some(UnlockBackendKind::PrivateKeyFile) => {
            seal_private_keyfile(dir, purpose, secret)?;
            Ok(UnlockBackendKind::PrivateKeyFile)
        }
        Some(other) => Err(format!(
            "cannot enroll `{other}` directly as a host keystore"
        )),
        None => {
            #[cfg(target_os = "linux")]
            if seal_systemd_cred(dir, purpose, secret).is_ok() {
                return Ok(UnlockBackendKind::SystemdCreds);
            }
            #[cfg(target_os = "macos")]
            if seal_macos_keychain(dir, purpose, secret).is_ok() {
                return Ok(UnlockBackendKind::MacosKeychain);
            }
            #[cfg(windows)]
            if seal_windows_dpapi(dir, purpose, secret).is_ok() {
                return Ok(UnlockBackendKind::WindowsDpapi);
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            if resolve_dbus_session_bus().is_some()
                && seal_secret_service(dir, purpose, secret).is_ok()
            {
                return Ok(UnlockBackendKind::SecretService);
            }
            seal_private_keyfile(dir, purpose, secret)?;
            Ok(UnlockBackendKind::PrivateKeyFile)
        }
    }
}

/// Generate or enroll the local vault master key and store it unattended on this host.
pub fn enroll_master_key(
    dir: &Path,
    preferred_backend: Option<&str>,
    key_override: Option<&str>,
) -> Result<UnlockBackendKind, String> {
    let backend = match preferred_backend
        .map(str::trim)
        .filter(|b| !b.is_empty() && !b.eq_ignore_ascii_case("auto"))
    {
        Some(raw) => Some(UnlockBackendKind::parse(raw)?),
        None => None,
    };
    let secret = if let Some(over) = key_override.map(str::trim).filter(|s| !s.is_empty()) {
        over.to_string()
    } else if let Some((existing, _)) = resolve_unlock_secret(dir, PURPOSE_VAULT)? {
        existing
    } else {
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).map_err(|e| format!("generate vault master key: {e}"))?;
        let hex = susi_paths::hex::encode(raw);
        wipe_bytes(&mut raw);
        hex
    };

    let enrolled = seal_secret_for_host(dir, PURPOSE_VAULT, &secret, backend)?;
    let master = normalize_master_key(&secret);
    drop_secret_string(secret);

    let mut cfg = VaultConfig::load(dir);
    cfg.record_backend(PURPOSE_VAULT, enrolled);
    cfg.save(dir)?;

    let sealed_path = SealedVault::path_in(dir);
    if !sealed_path.is_file() {
        let vault = SealedVault::default();
        let res = vault.save_with_key(&sealed_path, &master);
        let mut copy = master;
        wipe_bytes(&mut copy);
        res?;
    } else {
        let mut copy = master;
        let check = SealedVault::open_with_key(&sealed_path, &copy);
        wipe_bytes(&mut copy);
        check?;
    }
    Ok(enrolled)
}

/// Enroll the cross-OS KeePassXC `.kdbx` passphrase on this host for non-interactive syncs.
pub fn enroll_kdbx_passphrase(
    dir: &Path,
    passphrase: &str,
    preferred_backend: Option<&str>,
) -> Result<UnlockBackendKind, String> {
    if passphrase.trim().is_empty() {
        return Err("KeePassXC passphrase must not be empty".to_string());
    }
    let backend = match preferred_backend
        .map(str::trim)
        .filter(|b| !b.is_empty() && !b.eq_ignore_ascii_case("auto"))
    {
        Some(raw) => Some(UnlockBackendKind::parse(raw)?),
        None => None,
    };
    let enrolled = seal_secret_for_host(dir, PURPOSE_KDBX, passphrase, backend)?;
    let mut cfg = VaultConfig::load(dir);
    cfg.record_backend(PURPOSE_KDBX, enrolled);
    cfg.save(dir)?;
    Ok(enrolled)
}

/// Seal a single credential secret for `cloud-credentials/*.json`.
/// Returns `Ok(None)` when no unattended vault key is available on this host yet.
pub fn seal_credential_secret(dir: &Path, plaintext: &str) -> Result<Option<String>, String> {
    let Some(mut master) = resolve_master_key(dir)? else {
        return Ok(None);
    };
    let mut nonce = [0u8; 12];
    if let Err(e) = getrandom::fill(&mut nonce) {
        wipe_bytes(&mut master);
        return Err(format!("generate credential nonce: {e}"));
    }
    let mut subkey = derive_cred_subkey(&master, &nonce);
    wipe_bytes(&mut master);
    let cipher = ChaCha20Poly1305::new(Key::from(&subkey));
    wipe_bytes(&mut subkey);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: CRED_AAD,
            },
        )
        .map_err(|_| "seal credential AEAD encryption failed".to_string())?;
    Ok(Some(format!(
        "{SEALED_VALUE_PREFIX}{}:{}",
        susi_paths::hex::encode(nonce),
        susi_paths::hex::encode(&ct)
    )))
}

/// Unseal a `cloud-credentials/*.json` value if prefixed with `enc:v1:`; otherwise pass through.
pub fn unseal_credential_secret(dir: &Path, stored: &str) -> Result<String, String> {
    let Some(rest) = stored.strip_prefix(SEALED_VALUE_PREFIX) else {
        return Ok(stored.to_string());
    };
    let (nonce_hex, ct_hex) = rest
        .split_once(':')
        .ok_or_else(|| "malformed sealed credential envelope".to_string())?;
    let nonce = susi_paths::hex::decode(nonce_hex)
        .map_err(|e| format!("invalid sealed credential nonce: {e}"))?;
    if nonce.len() != 12 {
        return Err("sealed credential nonce must be 12 bytes".to_string());
    }
    let nonce_arr: [u8; 12] = nonce
        .as_slice()
        .try_into()
        .map_err(|_| "invalid nonce size".to_string())?;
    let ct = susi_paths::hex::decode(ct_hex)
        .map_err(|e| format!("invalid sealed credential ciphertext: {e}"))?;
    let mut master = resolve_master_key(dir)?.ok_or_else(|| {
        "cannot unseal credential without SUSI_VAULT_KEY or host unlock key".to_string()
    })?;
    let mut subkey = derive_cred_subkey(&master, &nonce_arr);
    wipe_bytes(&mut master);
    let cipher = ChaCha20Poly1305::new(Key::from(&subkey));
    wipe_bytes(&mut subkey);
    let mut pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce_arr),
            Payload {
                msg: &ct,
                aad: CRED_AAD,
            },
        )
        .map_err(|_| "sealed credential authentication failed".to_string())?;
    let out =
        String::from_utf8(pt.clone()).map_err(|_| "unsealed credential is not UTF-8".to_string());
    wipe_bytes(&mut pt);
    out
}

fn file_stamp_string(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some(format!("{}:{}:{mtime_nanos}", file_node(&meta), meta.len()))
}

fn compute_vault_cache_stamp(dir: &Path) -> VaultCacheStamp {
    let sealed_path = SealedVault::path_in(dir);
    let file_sig = file_stamp_string(&sealed_path);

    let mut unlock_hasher = Sha256::new();
    for name in [
        "vault.unlock.cred",
        "vault.unlock.dpapi",
        "vault.unlock.secretservice",
        "vault.unlock.secret",
        "vault.unlock.keychain",
        "vault.unlock.key",
    ] {
        unlock_hasher.update(name.as_bytes());
        unlock_hasher.update(b"=");
        if let Some(sig) = file_stamp_string(&dir.join(name)) {
            unlock_hasher.update(sig.as_bytes());
        }
        unlock_hasher.update(b"\n");
    }

    let mut env_hasher = Sha256::new();
    for var in [
        VAULT_KEY_ENV,
        VAULT_PASSPHRASE_ENV,
        VAULT_KEY_FILE_ENV,
        VAULT_PASSPHRASE_FILE_ENV,
        "CREDENTIALS_DIRECTORY",
        SUSI_CREDENTIALS_DIR_ENV,
    ] {
        env_hasher.update(var.as_bytes());
        env_hasher.update(b"=");
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            env_hasher.update(trimmed.as_bytes());
            if matches!(
                var,
                VAULT_KEY_FILE_ENV
                    | VAULT_PASSPHRASE_FILE_ENV
                    | "CREDENTIALS_DIRECTORY"
                    | SUSI_CREDENTIALS_DIR_ENV
            ) && !trimmed.is_empty()
            {
                let p = Path::new(trimmed);
                if let Some(sig) = file_stamp_string(p) {
                    env_hasher.update(b"@");
                    env_hasher.update(sig.as_bytes());
                }
                for fname in ["susi-vault-key", "vault.key", "susi-vault-passphrase"] {
                    if let Some(sig) = file_stamp_string(&p.join(fname)) {
                        env_hasher.update(b"@");
                        env_hasher.update(fname.as_bytes());
                        env_hasher.update(b":");
                        env_hasher.update(sig.as_bytes());
                    }
                }
            }
        }
        env_hasher.update(b"\n");
    }
    #[cfg(not(test))]
    for sys_dir in ["/run/secrets", "/var/run/secrets/susi"] {
        for fname in ["susi-vault-key", "vault.key", "susi-vault-passphrase"] {
            if let Some(sig) = file_stamp_string(&Path::new(sys_dir).join(fname)) {
                env_hasher.update(sys_dir.as_bytes());
                env_hasher.update(b"/");
                env_hasher.update(fname.as_bytes());
                env_hasher.update(b":");
                env_hasher.update(sig.as_bytes());
            }
        }
    }

    VaultCacheStamp {
        generation: VAULT_CACHE_GENERATION.load(Ordering::SeqCst),
        file_sig,
        unlock_digest: unlock_hasher.finalize().into(),
        env_digest: env_hasher.finalize().into(),
    }
}

fn warn_vault_unlock_once(dir: &Path, err: &str) {
    let key = dir.to_path_buf();
    let map = UNLOCK_WARN_ONCE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = map.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.get(&key).map(String::as_str) != Some(err) {
        guard.insert(key, err.to_string());
        eprintln!(
            "[susi-vault] failed to unlock {}: {err}",
            SealedVault::path_in(dir).display()
        );
    }
}

/// Resolve all `"env"` + `"cloud"` scope secrets from `dir/vault.sealed.json` when available.
/// Uses an mtime+size+env-validated cache to avoid repeated OS keystore subprocess spawns,
/// invalidating automatically on any vault edit or credential rotation.
#[must_use]
pub fn resolve_cloud_env_map(dir: &Path) -> Vec<(String, String)> {
    let cache_key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let stamp = compute_vault_cache_stamp(dir);
    let cache = CLOUD_ENV_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&cache_key)
        .filter(|entry| entry.stamp == stamp)
        .cloned()
    {
        return hit.entries;
    }

    let opened = SealedVault::open(dir);
    let vault = match opened {
        Ok(Some(v)) => v,
        Ok(None) => {
            cache.lock().unwrap_or_else(PoisonError::into_inner).insert(
                cache_key,
                CachedCloudEnv {
                    stamp,
                    entries: Vec::new(),
                },
            );
            return Vec::new();
        }
        Err(err) => {
            warn_vault_unlock_once(dir, &err);
            return Vec::new();
        }
    };
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in vault.scope_entries("env") {
        if !v.is_empty() {
            merged.insert(k, v);
        }
    }
    for (k, v) in vault.scope_entries("cloud") {
        if !v.is_empty() {
            merged.insert(k, v);
        }
    }
    let out: Vec<(String, String)> = merged.into_iter().collect();
    cache.lock().unwrap_or_else(PoisonError::into_inner).insert(
        cache_key,
        CachedCloudEnv {
            stamp,
            entries: out.clone(),
        },
    );
    out
}

/// Try persisting a cloud API key into `dir/vault.sealed.json` if an unattended master key exists.
/// Returns `Ok(true)` if written into the sealed vault, `Ok(false)` if un-enrolled.
pub fn try_store_cloud_key(dir: &Path, env_name: &str, value: &str) -> Result<bool, String> {
    let has_key = resolve_master_key(dir)?.is_some();
    if !has_key {
        return Ok(false);
    }
    let mut vault = SealedVault::open_or_init(dir)?;
    vault.set("cloud", env_name, value);
    vault.save(dir)?;
    Ok(true)
}

/// Remove a cloud API key from `dir/vault.sealed.json` if present and unlockable.
pub fn try_remove_cloud_key(dir: &Path, env_name: &str) -> Result<bool, String> {
    let Some(mut vault) = SealedVault::open(dir)? else {
        return Ok(false);
    };
    let removed = vault.remove("cloud", env_name) | vault.remove("env", env_name);
    if removed {
        vault.save(dir)?;
    }
    Ok(removed)
}

/// Move plaintext keys from `dir/cloud.env` into `dir/vault.sealed.json` (`"cloud"` scope).
/// Ensures a master unlock key is enrolled first if needed.
pub fn migrate_plaintext_cloud_env(
    dir: &Path,
    keep_plaintext: bool,
) -> Result<MigrationSummary, String> {
    if resolve_master_key(dir)?.is_none() {
        enroll_master_key(dir, None, None)?;
    }
    let cloud_env_path = dir.join("cloud.env");
    let raw = match std::fs::read_to_string(&cloud_env_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {e}", cloud_env_path.display())),
    };
    let parsed = parse_env_file(&raw);
    let mut vault = SealedVault::open_or_init(dir)?;
    let mut migrated = Vec::new();
    for (key, value) in parsed {
        if !key.trim().is_empty() && !value.trim().is_empty() {
            vault.set("cloud", &key, &value);
            migrated.push(key);
        }
    }
    vault.save(dir)?;

    let cleared = if !keep_plaintext && cloud_env_path.is_file() {
        atomic_write_bytes(
            &cloud_env_path,
            b"# Secrets migrated to ~/.susi/vault.sealed.json\n",
        )
        .map_err(|e| format!("clear plaintext cloud.env: {e}"))?;
        true
    } else {
        false
    };
    Ok(MigrationSummary {
        migrated_keys: migrated,
        cloud_env_cleared: cleared,
    })
}

/// Compute safe diagnostic status of `dir` without ever releasing secret values.
#[must_use]
pub fn vault_status(dir: &Path) -> VaultStatus {
    let cfg = VaultConfig::load(dir);
    let sealed_path = SealedVault::path_in(dir);
    let sealed_vault_exists = sealed_path.is_file();
    let unlock_info = resolve_unlock_secret(dir, PURPOSE_VAULT);
    let (unlock_backend, fp, unlock_error) = match unlock_info {
        Ok(Some((mut secret, kind))) => {
            let mut key = normalize_master_key(&secret);
            wipe_string(&mut secret);
            let fp = key_fingerprint(&key);
            wipe_bytes(&mut key);
            (Some(kind.as_str().to_string()), Some(fp), None)
        }
        Ok(None) => (None, None, None),
        Err(e) => (None, None, Some(e)),
    };

    let (unlocked, sealed_entry_count, open_err) = if sealed_vault_exists {
        match SealedVault::open(dir) {
            Ok(Some(v)) => (true, v.entry_count(), None),
            Ok(None) => (false, 0, None),
            Err(e) => (false, 0, Some(e)),
        }
    } else {
        (unlock_backend.is_some(), 0, None)
    };

    let (kdbx_unlock_enrolled, kdbx_unlock_backend) = match resolve_unlock_secret(dir, PURPOSE_KDBX)
    {
        Ok(Some((mut s, kind))) => {
            wipe_string(&mut s);
            (true, Some(kind.as_str().to_string()))
        }
        Ok(None) | Err(_) => (false, None),
    };

    let plaintext_cloud_env_keys = std::fs::read_to_string(dir.join("cloud.env"))
        .ok()
        .map(|text| parse_env_file(&text).len())
        .unwrap_or(0);

    VaultStatus {
        config_dir: dir.to_path_buf(),
        sealed_vault_exists,
        unlocked,
        unlock_backend,
        unlock_error: unlock_error.or(open_err),
        key_fingerprint: fp,
        sealed_entry_count,
        kdbx_unlock_enrolled,
        kdbx_unlock_backend,
        vault_db_path: cfg.vault_db_path,
        gdrive_sync_path: cfg.gdrive_sync_path,
        rclone_remote: cfg.rclone_remote,
        last_sync_unix: cfg.last_sync_unix,
        plaintext_cloud_env_keys,
    }
}

#[cfg(test)]
// Unit-test module only; production code in this module uses explicit Result handling.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_vault_store_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sealed_vault_roundtrips_with_aead_and_rejects_wrong_key_or_tamper() {
        let dir = temp_dir("roundtrip");
        let path = SealedVault::path_in(&dir);
        let key = normalize_master_key("correct-horse-battery-staple-test");
        let wrong_key = normalize_master_key("wrong-key-test");

        let mut vault = SealedVault::default();
        vault.set("cloud", "OPENAI_API_KEY", "sk-sealed-openai-test");
        vault.set("commercial", "SUSI_COMMERCIAL_SECRET", "comm-secret-123456");
        vault.save_with_key(&path, &key).unwrap();

        let disk_text = std::fs::read_to_string(&path).unwrap();
        assert!(!disk_text.contains("sk-sealed-openai-test"));
        assert!(!disk_text.contains("comm-secret-123456"));
        assert!(disk_text.contains(SEALED_VAULT_FORMAT));

        let reopened = SealedVault::open_with_key(&path, &key).unwrap().unwrap();
        assert_eq!(
            reopened.get("cloud", "OPENAI_API_KEY"),
            Some("sk-sealed-openai-test")
        );
        assert_eq!(
            reopened.get("commercial", "SUSI_COMMERCIAL_SECRET"),
            Some("comm-secret-123456")
        );
        assert_eq!(reopened.list_keys().len(), 2);

        let wrong_err = SealedVault::open_with_key(&path, &wrong_key).unwrap_err();
        assert!(wrong_err.contains("fingerprint mismatch"));

        // Corrupt the ciphertext while keeping matching fingerprint/envelope.
        let mut parsed: serde_json::Value = serde_json::from_str(&disk_text).unwrap();
        let ct = parsed["ciphertext_hex"].as_str().unwrap().to_string();
        let flipped = if let Some(rest) = ct.strip_prefix('0') {
            format!("f{rest}")
        } else {
            let tail = ct.strip_prefix(|_| true).unwrap_or("");
            format!("0{tail}")
        };
        parsed["ciphertext_hex"] = serde_json::json!(flipped);
        std::fs::write(&path, serde_json::to_string(&parsed).unwrap()).unwrap();
        let tamper_err = SealedVault::open_with_key(&path, &key).unwrap_err();
        assert!(tamper_err.contains("authentication failed"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyfile_enroll_and_credential_sealing_work_unattended() {
        let _guard = crate::env_test_lock();
        let dir = temp_dir("enroll");
        let prev = std::env::var_os(VAULT_KEY_ENV);
        std::env::remove_var(VAULT_KEY_ENV);

        let backend = enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        assert_eq!(backend, UnlockBackendKind::PrivateKeyFile);

        let sealed = seal_credential_secret(&dir, "sk-live-secret-999")
            .unwrap()
            .expect("keyfile backend should seal credentials");
        assert!(sealed.starts_with(SEALED_VALUE_PREFIX));
        assert!(!sealed.contains("sk-live-secret-999"));
        assert_eq!(
            unseal_credential_secret(&dir, &sealed).unwrap(),
            "sk-live-secret-999"
        );
        assert_eq!(
            unseal_credential_secret(&dir, "legacy-plain").unwrap(),
            "legacy-plain"
        );

        assert!(try_store_cloud_key(&dir, "DEEPSEEK_API_KEY", "ds-secret-42").unwrap());
        let cloud_map = resolve_cloud_env_map(&dir);
        assert_eq!(
            cloud_map,
            vec![("DEEPSEEK_API_KEY".to_string(), "ds-secret-42".to_string())]
        );

        let status = vault_status(&dir);
        assert!(status.sealed_vault_exists);
        assert!(status.unlocked);
        assert_eq!(status.unlock_backend.as_deref(), Some("keyfile"));
        assert_eq!(status.sealed_entry_count, 1);

        if let Some(v) = prev {
            std::env::set_var(VAULT_KEY_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_plaintext_cloud_env_seals_and_clears_legacy_file() {
        let _guard = crate::env_test_lock();
        let dir = temp_dir("migrate");
        let prev = std::env::var_os(VAULT_KEY_ENV);
        std::env::remove_var(VAULT_KEY_ENV);

        std::fs::write(
            dir.join("cloud.env"),
            "OPENAI_API_KEY=\"sk-migrate-1\"\nHF_TOKEN=hf_migrate_2\n",
        )
        .unwrap();
        enroll_master_key(&dir, Some("keyfile"), None).unwrap();

        let summary = migrate_plaintext_cloud_env(&dir, false).unwrap();
        assert!(summary.cloud_env_cleared);
        assert_eq!(
            summary.migrated_keys,
            vec!["OPENAI_API_KEY".to_string(), "HF_TOKEN".to_string()]
        );
        let cleared = std::fs::read_to_string(dir.join("cloud.env")).unwrap();
        assert!(!cleared.contains("sk-migrate-1"));
        assert!(!cleared.contains("hf_migrate_2"));

        let cloud_map = resolve_cloud_env_map(&dir);
        assert!(cloud_map.contains(&("OPENAI_API_KEY".to_string(), "sk-migrate-1".to_string())));
        assert!(cloud_map.contains(&("HF_TOKEN".to_string(), "hf_migrate_2".to_string())));

        if let Some(v) = prev {
            std::env::set_var(VAULT_KEY_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_creds_user_unattended_roundtrip_when_available() {
        let _guard = crate::env_test_lock();
        let dir = temp_dir("systemd_creds");
        let prev_vault = std::env::var_os(VAULT_KEY_ENV);
        let prev_kdbx = std::env::var_os(KDBX_PASSPHRASE_ENV);
        std::env::remove_var(VAULT_KEY_ENV);
        std::env::remove_var(KDBX_PASSPHRASE_ENV);

        if let Ok(backend) = enroll_master_key(&dir, Some("systemd-creds"), None) {
            assert_eq!(backend, UnlockBackendKind::SystemdCreds);
            let kdbx_backend =
                enroll_kdbx_passphrase(&dir, "cross-os-kdbx-test-pass", Some("systemd-creds"))
                    .unwrap();
            assert_eq!(kdbx_backend, UnlockBackendKind::SystemdCreds);

            let (recovered, kind) = resolve_unlock_secret(&dir, PURPOSE_KDBX).unwrap().unwrap();
            assert_eq!(kind, UnlockBackendKind::SystemdCreds);
            assert_eq!(recovered, "cross-os-kdbx-test-pass");

            assert!(try_store_cloud_key(&dir, "ANTHROPIC_API_KEY", "sk-ant-systemd-1").unwrap());
            let map = resolve_cloud_env_map(&dir);
            assert_eq!(
                map,
                vec![(
                    "ANTHROPIC_API_KEY".to_string(),
                    "sk-ant-systemd-1".to_string()
                )]
            );
        }

        if let Some(v) = prev_vault {
            std::env::set_var(VAULT_KEY_ENV, v);
        }
        if let Some(v) = prev_kdbx {
            std::env::set_var(KDBX_PASSPHRASE_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn container_secret_dirs_unlock_without_tty() {
        let dir = temp_dir("container_secrets");
        let run_secrets = dir.join("run_secrets");
        std::fs::create_dir_all(&run_secrets).unwrap();
        std::fs::write(
            run_secrets.join("susi-vault-key"),
            "k8s-sealed-master-key\n",
        )
        .unwrap();
        std::fs::write(
            run_secrets.join("susi-kdbx-passphrase"),
            "container-kdbx-pass\n",
        )
        .unwrap();

        let (vk, vb) = resolve_from_secret_dirs(PURPOSE_VAULT, std::slice::from_ref(&run_secrets))
            .unwrap()
            .unwrap();
        assert_eq!(vk, "k8s-sealed-master-key");
        assert_eq!(vb, UnlockBackendKind::CredentialsDirectory);

        let (kp, kb) = resolve_from_secret_dirs(PURPOSE_KDBX, &[run_secrets])
            .unwrap()
            .unwrap();
        assert_eq!(kp, "container-kdbx-pass");
        assert_eq!(kb, UnlockBackendKind::CredentialsDirectory);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passphrase_and_susi_credentials_directory_unlock_cross_os() {
        let _guard = crate::env_test_lock();
        let dir = temp_dir("passphrase_creds");
        let cred_dir = dir.join("creds");
        std::fs::create_dir_all(&cred_dir).unwrap();
        std::fs::write(cred_dir.join("susi-vault-key"), "dir-pass-123\n").unwrap();

        let prev_vk = std::env::var_os(VAULT_KEY_ENV);
        let prev_vp = std::env::var_os(VAULT_PASSPHRASE_ENV);
        let prev_cd = std::env::var_os("CREDENTIALS_DIRECTORY");
        let prev_scd = std::env::var_os(SUSI_CREDENTIALS_DIR_ENV);
        std::env::remove_var(VAULT_KEY_ENV);
        std::env::remove_var(VAULT_PASSPHRASE_ENV);
        std::env::remove_var("CREDENTIALS_DIRECTORY");
        std::env::set_var(SUSI_CREDENTIALS_DIR_ENV, &cred_dir);

        let (s1, b1) = resolve_unlock_secret(&dir, PURPOSE_VAULT).unwrap().unwrap();
        assert_eq!(s1, "dir-pass-123");
        assert_eq!(b1, UnlockBackendKind::CredentialsDirectory);

        std::env::set_var(VAULT_PASSPHRASE_ENV, "env-passphrase-456");
        let (s2, b2) = resolve_unlock_secret(&dir, PURPOSE_VAULT).unwrap().unwrap();
        assert_eq!(s2, "env-passphrase-456");
        assert_eq!(b2, UnlockBackendKind::EnvBootstrap);

        for (name, val) in [
            (VAULT_KEY_ENV, prev_vk),
            (VAULT_PASSPHRASE_ENV, prev_vp),
            ("CREDENTIALS_DIRECTORY", prev_cd),
            (SUSI_CREDENTIALS_DIR_ENV, prev_scd),
        ] {
            match val {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_primary_unlock_falls_through_to_keyfile() {
        let _guard = crate::env_test_lock();
        let dir = temp_dir("fallthrough");
        let prev = std::env::var_os(VAULT_KEY_ENV);
        let prev_p = std::env::var_os(VAULT_PASSPHRASE_ENV);
        std::env::remove_var(VAULT_KEY_ENV);
        std::env::remove_var(VAULT_PASSPHRASE_ENV);

        seal_private_keyfile(&dir, PURPOSE_VAULT, "fallback-keyfile-secret").unwrap();
        #[cfg(target_os = "linux")]
        std::fs::write(cred_file_path(&dir, PURPOSE_VAULT), b"corrupt-cred").unwrap();
        #[cfg(all(unix, not(target_os = "macos")))]
        std::fs::write(secret_service_marker_path(&dir, PURPOSE_VAULT), "").unwrap();

        let (secret, backend) = resolve_unlock_secret(&dir, PURPOSE_VAULT).unwrap().unwrap();
        assert_eq!(secret, "fallback-keyfile-secret");
        assert_eq!(backend, UnlockBackendKind::PrivateKeyFile);

        match prev {
            Some(v) => std::env::set_var(VAULT_KEY_ENV, v),
            None => std::env::remove_var(VAULT_KEY_ENV),
        }
        match prev_p {
            Some(v) => std::env::set_var(VAULT_PASSPHRASE_ENV, v),
            None => std::env::remove_var(VAULT_PASSPHRASE_ENV),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn macos_keychain_and_windows_powershell_helpers_are_well_formed() {
        let dir = temp_dir("helpers");
        assert_eq!(
            macos_keychain_marker_path(&dir, PURPOSE_VAULT),
            dir.join("vault.unlock.keychain")
        );
        let _ = std::fs::remove_dir_all(&dir);

        let mut script = macos_keychain_add_script("susi-vault-abc", "s e c\nr e t");
        assert!(script.starts_with(
            "add-generic-password -T /usr/bin/security -U -s susi-vault -a susi-vault-abc -w hex:"
        ));
        let payload = script.rsplit("-w ").next().unwrap();
        assert_eq!(
            decode_secret_payload(payload, "test").unwrap(),
            "s e c\nr e t"
        );
        wipe_string(&mut script);

        assert_eq!(
            windows_powershell_exe_from(Some(r"C:\Windows")),
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        );
        assert_eq!(
            UnlockBackendKind::parse("keychain").unwrap(),
            UnlockBackendKind::MacosKeychain
        );
    }

    #[test]
    fn cloud_env_cache_invalidates_on_update() {
        let _guard = crate::env_test_lock();
        let dir = temp_dir("cache_invalidate");
        let prev = std::env::var_os(VAULT_KEY_ENV);
        std::env::remove_var(VAULT_KEY_ENV);

        enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        try_store_cloud_key(&dir, "OPENAI_API_KEY", "sk-first").unwrap();
        assert_eq!(
            resolve_cloud_env_map(&dir),
            vec![("OPENAI_API_KEY".to_string(), "sk-first".to_string())]
        );

        try_store_cloud_key(&dir, "OPENAI_API_KEY", "sk-second").unwrap();
        assert_eq!(
            resolve_cloud_env_map(&dir),
            vec![("OPENAI_API_KEY".to_string(), "sk-second".to_string())]
        );

        if let Some(v) = prev {
            std::env::set_var(VAULT_KEY_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dbus_session_bus_resolution_handles_env_xdg_and_run_user_uid() {
        assert_eq!(
            resolve_dbus_session_bus_with(
                Some("unix:path=/tmp/custom-bus"),
                Some("/run/user/1000"),
                None,
                None,
                |_| false,
            ),
            Some("unix:path=/tmp/custom-bus".to_string())
        );

        assert_eq!(
            resolve_dbus_session_bus_with(None, Some("/run/user/1001/"), None, None, |p| p
                == Path::new("/run/user/1001/bus"),),
            Some("unix:path=/run/user/1001/bus".to_string())
        );

        let proc_status = "Name:\tsusi\nUid:\t1002\t1002\t1002\t1002\nGid:\t1002\t1002\n";
        assert_eq!(
            resolve_dbus_session_bus_with(None, None, Some(proc_status), Some("9999"), |p| p
                == Path::new("/run/user/1002/bus"),),
            Some("unix:path=/run/user/1002/bus".to_string())
        );

        assert_eq!(
            resolve_dbus_session_bus_with(None, None, None, Some("1003"), |p| {
                p == Path::new("/run/user/1003/bus")
            }),
            Some("unix:path=/run/user/1003/bus".to_string())
        );

        assert_eq!(
            resolve_dbus_session_bus_with(None, None, None, None, |_| true),
            None
        );
    }
}
