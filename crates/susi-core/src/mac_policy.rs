//! Mandatory Access Control (MAC) for agent tool I/O and egress.
//!
//! Cryptographic capability tokens (HMAC-SHA256) bound to subject / action /
//! resource / expiry. Composition root loads the host key and privacy mode;
//! every tool dispatch must pass [`MacPolicy::authorize_tool`].

use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::sync::RwLock;

const HMAC_BLOCK: usize = 64;

/// Privacy posture for the substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    /// Read/write/exec allowed for host; network egress still needs a grant.
    #[default]
    Balanced,
    /// No cloud inference, no network egress, host exec forced through sandbox
    /// unless a capability token explicitly allows otherwise.
    LocalOnly,
    /// Permissive: default grants include network.egress (still audited).
    Open,
}

impl PrivacyMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "local_only" | "local-only" | "edge" | "strict" => Self::LocalOnly,
            "open" | "permissive" => Self::Open,
            _ => Self::Balanced,
        }
    }

    /// Exact parse of a persisted mode (`as_str` output). The sticky file
    /// is machine-written, so anything else — empty, torn, garbled — is
    /// damage, not a request for the lenient `parse` default.
    pub fn parse_persisted(s: &str) -> Option<Self> {
        match s.trim() {
            "balanced" => Some(Self::Balanced),
            "local_only" => Some(Self::LocalOnly),
            "open" => Some(Self::Open),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::LocalOnly => "local_only",
            Self::Open => "open",
        }
    }
}

/// Canonical capability actions (align with extension-pack vocabulary).
pub mod actions {
    pub const FILESYSTEM_READ: &str = "filesystem.read";
    pub const FILESYSTEM_WRITE: &str = "filesystem.write";
    pub const PROCESS_EXEC: &str = "process.exec";
    pub const SANDBOX_EXEC: &str = "sandbox.exec";
    pub const NETWORK_EGRESS: &str = "network.egress";
    pub const TRANSMIT: &str = "transmit";
    pub const CLOUD_INFERENCE: &str = "cloud.inference";
}

/// HMAC-signed capability grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityToken {
    pub subject: String,
    pub action: String,
    pub resource: String,
    pub issued_at: u64,
    pub expires_at: Option<u64>,
    /// Hex-encoded HMAC-SHA256 over the canonical payload.
    pub signature: String,
}

impl CapabilityToken {
    fn canonical_payload(&self) -> String {
        format!(
            "{}|{}|{}",
            grant_key(&self.subject, &self.action, &self.resource),
            self.issued_at,
            self.expires_at
                .map(|e| e.to_string())
                .unwrap_or_else(|| "none".into())
        )
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|exp| exp <= now)
    }
}

fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    let mut keyed = [0u8; HMAC_BLOCK];
    keyed[..32].copy_from_slice(key);
    let mut ipad = [0x36u8; HMAC_BLOCK];
    let mut opad = [0x5cu8; HMAC_BLOCK];
    for i in 0..HMAC_BLOCK {
        ipad[i] ^= keyed[i];
        opad[i] ^= keyed[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut mac = [0u8; 32];
    mac.copy_from_slice(&out);
    mac
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn grant_key(subject: &str, action: &str, resource: &str) -> String {
    if [subject, action, resource]
        .iter()
        .any(|field| field.contains('|'))
    {
        // Hex fields cannot contain either separator. The resulting key has
        // zero pipes and cannot collide with the two-pipe legacy key.
        format!(
            "v2:{}:{}:{}",
            hex::encode(subject),
            hex::encode(action),
            hex::encode(resource)
        )
    } else {
        format!("{subject}|{action}|{resource}")
    }
}

/// Process-wide MAC enforcer.
pub struct MacPolicy {
    key: [u8; 32],
    grants: RwLock<HashMap<String, CapabilityToken>>,
    mode: RwLock<PrivacyMode>,
    mandatory_sandbox: AtomicBool,
    /// When true, every elevated check requires a verified token (always on).
    enforce: AtomicBool,
    /// Shared substrate state dir (set on the wired path): grants persist to
    /// `<dir>/mac.grants/` and privacy mode reads/writes `<dir>/privacy_mode`
    /// so sibling cell processes share policy state with this one.
    state_dir: Option<PathBuf>,
    /// A production instance without its shared signing key may evaluate its
    /// local-only defaults, but must never acknowledge a durable mutation.
    persistence_error: Option<String>,
}

static POLICY: OnceLock<MacPolicy> = OnceLock::new();

fn enc(key: &str) -> String {
    crate::susi_core::plane_bus_ipc::enc(key)
}

fn grant_paths(
    dir: &Path,
    subject: &str,
    action: &str,
    resource: &str,
) -> (PathBuf, Option<PathBuf>) {
    let mut hash = Sha256::new();
    for field in [subject, action, resource] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let current = dir.join(format!("v2-{}", hex::encode(hash.finalize())));
    let legacy = enc(&format!("{subject}|{action}|{resource}"));
    // Previous names were ASCII. Longer components cannot have existed on
    // the supported filesystems, and probing one would itself fail.
    let old = (legacy.len() <= 255).then(|| dir.join(legacy));
    (current, old)
}

/// Load the shared substrate HMAC key, creating it (0600) on first boot.
/// Every wired process — the daemon and each cell — signs/verifies with it, so
/// creation is race-safe and a malformed key is an error, never rotated.
pub fn load_or_create_key(dir: &Path) -> std::io::Result<[u8; 32]> {
    crate::susi_config::load_or_create_secret(&dir.join("mac.hmac.key"))
}

/// A key private to this process, for a policy that cannot join the
/// shared substrate key. OS entropy when available; otherwise a digest of
/// process-unique inputs — never an all-zero key.
/// The sticky posture under `dir`. Absent means "no override" (`None`).
/// Present but unreadable or unrecognized fails closed to `LocalOnly`: the
/// lenient parse used to turn an empty or torn file into `Balanced`, which
/// re-enabled cloud inference and host exec on a `local_only` host.
fn read_sticky_mode(dir: &Path) -> Option<PrivacyMode> {
    let path = dir.join("privacy_mode");
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(PrivacyMode::parse_persisted(&text).unwrap_or_else(|| {
            tracing::error!(path = %path.display(), "unrecognized privacy_mode; enforcing local_only");
            PrivacyMode::LocalOnly
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::error!(%error, path = %path.display(), "unreadable privacy_mode; enforcing local_only");
            Some(PrivacyMode::LocalOnly)
        }
    }
}

/// Whether `url` stays on this host or its LAN: loopback, private (RFC 1918
/// / unique-local), or link-local addresses, `localhost`, `*.localhost`,
/// `*.local`. Anything else leaves the network — including a plain-`http://`
/// internet host. Parsed with std only (every crate mounts this module).
pub fn url_stays_local(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_private())
        }
        Err(_) => {
            let host = host.to_ascii_lowercase();
            !host.is_empty()
                && (host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local"))
        }
    }
}

/// Whether host-initiated network traffic to `url` is allowed right now:
/// always for host/LAN targets; otherwise only when the posture does not
/// block network egress by default or an egress grant is held. For code
/// paths that reach the network without going through `authorize_tool`
/// (background downloads, provider calls).
pub fn egress_permitted(url: &str) -> bool {
    MacPolicy::global().egress_permitted(url)
}

/// Whether a grant on `granted` covers a request for `requested`.
///
/// Exact match; an explicit trailing `*` globs its prefix (`app://*`);
/// otherwise the request must sit *under* the grant at a `/` boundary.
/// A bare `starts_with` let a grant on `/srv/ws` authorize `/srv/ws2`,
/// and one on `https://api.example.com` authorize
/// `https://api.example.com.evil.net`.
pub fn grant_covers(granted: &str, requested: &str) -> bool {
    if granted == requested {
        return true;
    }
    if let Some(prefix) = granted.strip_suffix('*') {
        return requested.starts_with(prefix);
    }
    requested
        .strip_prefix(granted)
        .is_some_and(|rest| granted.ends_with('/') || rest.starts_with('/'))
}

/// `\\?\C:\x` becomes `C:\x` and `\\?\UNC\server\share` becomes
/// `\\server\share`; any other path is returned unchanged.
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

fn process_local_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    if getrandom::fill(&mut key).is_ok() {
        return key;
    }
    let seed = format!(
        "susi-mac-local:{}:{:?}:{}",
        std::process::id(),
        std::thread::current().id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    key.copy_from_slice(&Sha256::digest(seed.as_bytes()));
    key
}

impl MacPolicy {
    pub fn new(key: [u8; 32], mode: PrivacyMode, mandatory_sandbox: bool) -> Self {
        let p = Self {
            key,
            grants: RwLock::new(HashMap::new()),
            mode: RwLock::new(mode),
            mandatory_sandbox: AtomicBool::new(mandatory_sandbox),
            enforce: AtomicBool::new(true),
            state_dir: None,
            persistence_error: None,
        };
        if let Err(error) = p.seed_defaults() {
            tracing::error!(%error, "could not initialize in-memory MAC defaults");
        }
        p
    }

    /// Enable shared-state persistence: grants mirror to `<dir>/mac.grants/`
    /// and mode reads/writes `<dir>/privacy_mode`.
    fn with_state_dir(mut self, dir: PathBuf) -> Self {
        // Read persisted grants on demand so a sibling's revoke/renewal also
        // affects this already-running instance.
        self.state_dir = Some(dir);
        self
    }

    /// Initialize the global policy (first call wins). Wired via the
    /// substrate state dir so sibling processes share grants and mode.
    pub fn init_global(key: [u8; 32], mode: PrivacyMode, mandatory_sandbox: bool) -> &'static Self {
        POLICY.get_or_init(|| {
            Self::new(key, mode, mandatory_sandbox)
                .with_state_dir(susi_paths::SusiDirs::substrate_home())
        })
    }

    /// The wired constructor every cell uses for `global()`: shared key
    /// file, sticky mode, persisted grants — all under `substrate_home`.
    pub fn wired() -> Self {
        let dir = susi_paths::SusiDirs::substrate_home();
        let key = match load_or_create_key(&dir) {
            Ok(key) => key,
            Err(error) => {
                // No shared key: grants this copy signs cannot be verified
                // by the rest of the substrate, so fail closed rather than
                // run a permissive policy on a private key.
                tracing::error!(%error, dir = %dir.display(), "substrate MAC key unavailable; enforcing local_only");
                let mut local = Self::new(process_local_key(), PrivacyMode::LocalOnly, true);
                local.persistence_error =
                    Some(format!("shared MAC signing key is unavailable: {error}"));
                return local;
            }
        };
        let cfg = crate::susi_config::SusiConfig::load_global().unwrap_or_default();
        let privacy = cfg.privacy();
        // Mode: sticky file beats config default.
        let mode = read_sticky_mode(&dir).unwrap_or_else(|| PrivacyMode::parse(&privacy.mode));
        let mandatory =
            privacy.mandatory_sandbox_for_exec || matches!(mode, PrivacyMode::LocalOnly);
        Self::new(key, mode, mandatory).with_state_dir(dir)
    }

    pub fn global() -> &'static Self {
        POLICY.get_or_init(|| {
            // Production leaf crates embed this module independently and do
            // not pass through the daemon composition root. They must join
            // the shared substrate policy here; an ephemeral Balanced policy
            // silently bypasses a live `local_only` switch in those copies.
            #[cfg(not(test))]
            {
                Self::wired()
            }
            #[cfg(test)]
            {
                let mut key = [0u8; 32];
                let seed = format!("susi-mac-ephemeral:{}", std::process::id());
                let digest = Sha256::digest(seed.as_bytes());
                key.copy_from_slice(&digest[..32]);
                Self::new(key, PrivacyMode::Balanced, false)
            }
        })
    }

    fn grants_dir(&self) -> Option<PathBuf> {
        self.state_dir.as_ref().map(|d| d.join("mac.grants"))
    }

    fn locked_store(&self) -> std::io::Result<Option<(PathBuf, std::fs::File)>> {
        if let Some(error) = &self.persistence_error {
            return Err(std::io::Error::other(error.clone()));
        }
        let Some(dir) = self.grants_dir() else {
            return Ok(None);
        };
        crate::susi_config::create_private_dir(&dir)?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".records.lock"))?;
        lock.lock()?;
        Ok(Some((dir, lock)))
    }

    /// Writers hold the shared lock while atomically moving a legacy-only
    /// record to its bounded location. Its prior signed bytes are preserved.
    fn migrate_record(
        &self,
        dir: &Path,
        subject: &str,
        action: &str,
        resource: &str,
    ) -> std::io::Result<PathBuf> {
        let (current, legacy) = grant_paths(dir, subject, action, resource);
        if let Some(old) = legacy {
            let matches = match std::fs::read(&old) {
                Ok(bytes) => serde_json::from_slice::<CapabilityToken>(&bytes).is_ok_and(|t| {
                    t.subject == subject
                        && t.action == action
                        && t.resource == resource
                        && self.verify(&t)
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            };
            if matches {
                if current.try_exists()? {
                    std::fs::remove_file(old)?;
                } else {
                    std::fs::rename(old, &current)?;
                }
            }
        }
        Ok(current)
    }

    fn publish(&self, tokens: &[CapabilityToken]) -> std::io::Result<()> {
        let Some((dir, _lock)) = self.locked_store()? else {
            for token in tokens {
                self.grants
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        grant_key(&token.subject, &token.action, &token.resource),
                        token.clone(),
                    );
            }
            return Ok(());
        };
        let mut changes = Vec::new();
        for token in tokens {
            let path = self.migrate_record(&dir, &token.subject, &token.action, &token.resource)?;
            let before = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            changes.push((
                path,
                before,
                serde_json::to_vec(token).map_err(std::io::Error::other)?,
            ));
        }
        for (i, (path, _, bytes)) in changes.iter().enumerate() {
            if let Err(error) = crate::susi_config::atomic_write_bytes(path, bytes) {
                for (written, before, _) in changes[..i].iter().rev() {
                    let restored = match before {
                        Some(original) => crate::susi_config::atomic_write_bytes(written, original),
                        None => std::fs::remove_file(written),
                    };
                    if let Err(rollback) = restored {
                        return Err(std::io::Error::other(format!(
                            "grant publication failed ({error}); rollback failed ({rollback})"
                        )));
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn current_token(
        &self,
        subject: &str,
        action: &str,
        resource: &str,
    ) -> Option<CapabilityToken> {
        let matches = |token: &CapabilityToken| {
            token.subject == subject
                && token.action == action
                && token.resource == resource
                && self.verify(token)
        };
        if let Some(dir) = self.grants_dir() {
            let (current, old) = grant_paths(&dir, subject, action, resource);
            for path in std::iter::once(current).chain(old) {
                match std::fs::read(path) {
                    Ok(data) => {
                        return serde_json::from_slice::<CapabilityToken>(&data)
                            .ok()
                            .filter(matches)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return None,
                }
            }
        }
        self.grants
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&grant_key(subject, action, resource))
            .cloned()
            .filter(matches)
    }

    fn bindings_for(&self, subject: &str) -> std::collections::BTreeSet<(String, String)> {
        let mut bindings: std::collections::BTreeSet<_> = self
            .grants
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|item| item.1.subject == subject)
            .map(|item| (item.1.action.clone(), item.1.resource.clone()))
            .collect();
        if let Some(dir) = self.grants_dir() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    // A lock, tombstone or in-progress atomic writer is not an issued grant.
                    if entry.file_name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    let Some(token) = std::fs::read(entry.path())
                        .ok()
                        .and_then(|b| serde_json::from_slice::<CapabilityToken>(&b).ok())
                    else {
                        continue;
                    };
                    if token.subject == subject {
                        bindings.insert((token.action, token.resource));
                    }
                }
            }
        }
        bindings
    }

    fn sticky_mode(&self) -> Option<PrivacyMode> {
        read_sticky_mode(self.state_dir.as_ref()?)
    }

    pub fn mode(&self) -> PrivacyMode {
        // Wired copies share the sticky file so a mode switch in any copy is
        // authoritative everywhere; unwired copies use the in-memory value.
        if let Some(m) = self.sticky_mode() {
            return m;
        }
        *self.mode.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Switch the posture. For a wired policy the sticky file is what every
    /// other copy enforces, so it is replaced atomically and a failed write
    /// is returned: reporting a switch other processes never see would be a
    /// silent downgrade of the operator's intent.
    pub fn set_mode(&self, mode: PrivacyMode) -> std::io::Result<()> {
        // A failed downgrade must not advertise a new local-only mode while
        // a previous wildcard grant still permits the operation.
        self.revoke("susi", actions::NETWORK_EGRESS, "*")
            .map_err(std::io::Error::other)?;
        self.revoke("susi", actions::CLOUD_INFERENCE, "*")
            .map_err(std::io::Error::other)?;
        self.revoke("susi", actions::PROCESS_EXEC, "*")
            .map_err(std::io::Error::other)?;
        if let Some(dir) = self.state_dir.as_ref() {
            crate::susi_config::atomic_write_bytes(
                &dir.join("privacy_mode"),
                mode.as_str().as_bytes(),
            )?;
        }
        if let Ok(mut g) = self.mode.write() {
            *g = mode;
        }
        // Drop elevated grants so the new posture is authoritative.
        self.mandatory_sandbox
            .store(matches!(mode, PrivacyMode::LocalOnly), Ordering::Relaxed);
        self.seed_defaults().map_err(std::io::Error::other)?;
        Ok(())
    }

    /// Whether this policy shares its posture through the sticky file
    /// (`set_mode` then persists it); unwired copies keep it in memory.
    pub fn persists_mode(&self) -> bool {
        self.state_dir.is_some()
    }

    /// Whether host-initiated network traffic to `url` is allowed under this
    /// policy instance. Local targets are always permitted; non-local targets
    /// require an open posture or an explicit egress grant.
    pub fn egress_permitted(&self, url: &str) -> bool {
        if url_stays_local(url) {
            return true;
        }
        !self.blocks_network_by_default() || self.is_permitted("susi", actions::NETWORK_EGRESS, "*")
    }

    pub fn mandatory_sandbox(&self) -> bool {
        self.mandatory_sandbox.load(Ordering::Relaxed)
            || matches!(self.mode(), PrivacyMode::LocalOnly)
    }

    pub fn set_mandatory_sandbox(&self, on: bool) {
        self.mandatory_sandbox.store(on, Ordering::Relaxed);
    }

    /// True when cloud inference / remote providers must not receive payloads.
    pub fn blocks_cloud_inference(&self) -> bool {
        matches!(self.mode(), PrivacyMode::LocalOnly)
            && !self.is_permitted("susi", actions::CLOUD_INFERENCE, "*")
    }

    /// True when network egress tools must not run without a grant.
    pub fn blocks_network_by_default(&self) -> bool {
        !matches!(self.mode(), PrivacyMode::Open)
    }

    fn sign(&self, token: &CapabilityToken) -> String {
        hex::encode(hmac_sha256(&self.key, token.canonical_payload().as_bytes()))
    }

    pub fn verify(&self, token: &CapabilityToken) -> bool {
        if token.is_expired(now_secs()) {
            return false;
        }
        let expect = self.sign(token);
        // Constant-time-ish compare
        expect.len() == token.signature.len()
            && expect
                .as_bytes()
                .iter()
                .zip(token.signature.as_bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    fn issue_token(
        &self,
        subject: &str,
        action: &str,
        resource: &str,
        ttl_secs: Option<u64>,
    ) -> EaiResult<CapabilityToken> {
        let issued = now_secs();
        let mut token = CapabilityToken {
            subject: subject.into(),
            action: action.into(),
            resource: resource.into(),
            issued_at: issued,
            expires_at: ttl_secs
                .map(|s| {
                    issued.checked_add(s).ok_or_else(|| {
                        EaiError::config("capability expiry exceeds timestamp range")
                    })
                })
                .transpose()?,
            signature: String::new(),
        };
        token.signature = self.sign(&token);
        Ok(token)
    }

    /// Issue and store a capability token. An unwritable shared store never
    /// turns a failed grant into a successful process-local permission.
    pub fn grant(
        &self,
        subject: &str,
        action: &str,
        resource: &str,
        ttl_secs: Option<u64>,
    ) -> EaiResult<CapabilityToken> {
        let token = self.issue_token(subject, action, resource, ttl_secs)?;
        self.publish(std::slice::from_ref(&token))
            .map_err(|e| EaiError::io(format!("could not persist capability grant: {e}")))?;
        Ok(token)
    }

    pub fn revoke(&self, subject: &str, action: &str, resource: &str) -> EaiResult<bool> {
        self.revoke_record(subject, action, resource)
            .map_err(|e| EaiError::io(format!("could not revoke capability grant: {e}")))
    }

    fn revoke_record(&self, subject: &str, action: &str, resource: &str) -> std::io::Result<bool> {
        let key = grant_key(subject, action, resource);
        let Some((dir, _lock)) = self.locked_store()? else {
            return Ok(self
                .grants
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key)
                .is_some());
        };
        let current = self.migrate_record(&dir, subject, action, resource)?;
        let present = match std::fs::read(&current) {
            Ok(bytes) => serde_json::from_slice::<Option<CapabilityToken>>(&bytes)
                .map_err(std::io::Error::other)?
                .is_some(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self
                .grants
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&key),
            Err(error) => return Err(error),
        };
        if present {
            if subject == "susi" && matches!(resource, "*" | "app://*") {
                // New consumers seed implicit host defaults. A tombstone masks
                // this binding until it is explicitly granted again.
                crate::susi_config::atomic_write_bytes(&current, b"null")?;
            } else {
                std::fs::remove_file(&current)?;
            }
        }
        self.grants
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        Ok(present)
    }

    pub fn grants_for(&self, subject: &str) -> Vec<CapabilityToken> {
        self.bindings_for(subject)
            .into_iter()
            .filter_map(|(a, r)| self.current_token(subject, &a, &r))
            .collect()
    }

    /// Explicit operator consent for network egress (and optionally cloud).
    pub fn consent_egress(&self, subject: &str, ttl_secs: u64) -> EaiResult<Vec<CapabilityToken>> {
        let mut tokens =
            vec![self.issue_token(subject, actions::NETWORK_EGRESS, "*", Some(ttl_secs))?];
        if matches!(self.mode(), PrivacyMode::LocalOnly) {
            tokens.push(self.issue_token(
                subject,
                actions::CLOUD_INFERENCE,
                "*",
                Some(ttl_secs),
            )?);
        }
        self.publish(&tokens)
            .map_err(|e| EaiError::io(format!("could not persist egress consent: {e}")))?;
        Ok(tokens)
    }

    pub fn is_permitted(&self, subject: &str, action: &str, resource: &str) -> bool {
        if !self.enforce.load(Ordering::Relaxed) {
            return true;
        }
        let now = now_secs();
        let live = |scope: &str| {
            self.current_token(subject, action, scope)
                .is_some_and(|t| !t.is_expired(now))
        };
        if live(resource) || (resource != "*" && live("*")) {
            return true;
        }
        self.bindings_for(subject).into_iter().any(|(a, r)| {
            a == action && r != resource && r != "*" && grant_covers(&r, resource) && live(&r)
        })
    }

    fn seed_defaults(&self) -> EaiResult<()> {
        let mut bindings = vec![
            (actions::FILESYSTEM_READ, "*"),
            (actions::FILESYSTEM_WRITE, "*"),
            (actions::SANDBOX_EXEC, "*"),
            (actions::TRANSMIT, "app://*"),
        ];
        if !matches!(self.mode(), PrivacyMode::LocalOnly) {
            bindings.extend([
                (actions::PROCESS_EXEC, "*"),
                (actions::NETWORK_EGRESS, "*"),
                (actions::CLOUD_INFERENCE, "*"),
            ]);
        }
        let tokens = bindings
            .into_iter()
            .map(|(action, resource)| self.issue_token("susi", action, resource, None))
            .collect::<EaiResult<Vec<_>>>()?;
        self.publish(&tokens)
            .map_err(|e| EaiError::io(format!("could not store MAC defaults: {e}")))
    }

    /// Tools that send data off-host but whose names match none of the
    /// substring rules below; without this list they were classified as
    /// local reads and bypassed the egress gate under `local_only`.
    const EGRESS_TOOLS: &'static [&'static str] = &[
        "a2a_delegate",      // task to a remote A2A peer
        "agents_send",       // message to a cloud agent task
        "power_reason",      // delegation to remote MCP servers
        "meta_scout_agents", // capability listing from connected remotes
        "brave_search",      // web search (query leaves the host)
        "google_search",     // web search (query leaves the host)
        "provider_probe",    // completion through a (usually paid) endpoint
        "provider_pull",     // weights pulled from a model registry
        "brain_probe",       // scout benchmark against a provider
        "keys_check",        // each vendor's model list, with its key
        "keys_models",       // the vendor's model list, with its key
        "harness_author",    // plan authored through the metered gateway
        "harness_run",       // mission tasks through the metered gateway
        "harness_budget",    // provider balances fetched with their keys
        "harness_plan",      // provider balances fetched with their keys
    ];

    /// Extra requirements for storing an adapter document over the socket:
    /// a stored adapter is run later by `agents_run`, `frameworks_run`,
    /// retries or an MCP enable, so storing one that names a program needs
    /// `process.exec`, and storing a remote A2A agent needs
    /// `network.egress`. `kind` is the adapter kind the tool names.
    fn adapter_document_requirements(
        kind: Option<&str>,
        doc: &serde_json::Value,
    ) -> Vec<(&'static str, &'static str)> {
        let kind = kind
            .map(|k| k.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let kind = kind.trim_end_matches('s');
        match kind {
            "agent" | "framework" => {
                match doc.get("kind").and_then(|v| v.as_str()) {
                    Some("a2a") => vec![(actions::NETWORK_EGRESS, "*")],
                    // `command`, `acp`, or a shape the upgrade will refuse:
                    // fail closed.
                    _ => vec![(actions::PROCESS_EXEC, "*")],
                }
            }
            "mcp" => {
                let names_program = ["runner", "args", "package"]
                    .iter()
                    .any(|key| doc.get(*key).is_some_and(|v| !v.is_null()));
                if names_program {
                    vec![(actions::PROCESS_EXEC, "*")]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Map a tool name to required (action, resource) pairs.
    pub fn requirements_for_tool(tool: &str) -> Vec<(&'static str, &'static str)> {
        let t = tool.to_ascii_lowercase();
        // Network / cloud surfaces
        if Self::EGRESS_TOOLS.contains(&t.as_str())
            || t.contains("browser")
            || t.starts_with("mcp_")
            || t.starts_with("leading_mcp")
            || t.contains("scout_model")
            || t.contains("download")
            || t == "rag_query"
            || t.contains("http")
            || t.contains("fetch")
            || t.contains("web_search")
            || t.contains("live_search")
        {
            return vec![(actions::NETWORK_EGRESS, "*")];
        }
        if t == "exec_command" {
            return vec![(actions::PROCESS_EXEC, "*")];
        }
        if t == "sandbox_exec" {
            return vec![(actions::SANDBOX_EXEC, "*")];
        }
        if t == "write_file" || t == "apply_patch_cycle" {
            return vec![(actions::FILESYSTEM_WRITE, "*")];
        }
        if t == "read_file"
            || t.starts_with("context_graph")
            || t == "status"
            || t == "identity"
            || t == "host_telemetry"
            || t == "list_models"
        {
            return vec![(actions::FILESYSTEM_READ, "*")];
        }
        if t.starts_with("ipc_send") {
            // Local broker delivery: the scope the default `app://*`
            // transmit grant covers. Requiring `*` here matched no grant in
            // any mode, so ipc_send was MAC-denied before the broker's own
            // per-sender dispatch check ever ran.
            return vec![(actions::TRANSMIT, "app://*")];
        }
        if t == "reason" || t == "susi_solve" || t == "mission_run" || t.starts_with("agents_") {
            // Local swarm reasoning — read only at MAC layer; cloud blocked separately.
            return vec![(actions::FILESYSTEM_READ, "*")];
        }
        // Default: treat unknown tools as needing read (fail open-ish within local).
        vec![(actions::FILESYSTEM_READ, "*")]
    }

    /// Authorize a tool invocation for `subject` (defaults to `"susi"`).
    pub fn authorize_tool(
        &self,
        tool: &str,
        arg: &serde_json::Value,
        _workspace: &Path,
        subject: Option<&str>,
    ) -> EaiResult<()> {
        let subject = subject.unwrap_or("susi");

        // Local-only / mandatory sandbox: host exec may proceed only with
        // process.exec, otherwise CoreTools redirects to sandbox_exec — allow
        // when sandbox.exec is held.
        if tool == "exec_command" && self.mandatory_sandbox() {
            if self.is_permitted(subject, actions::PROCESS_EXEC, "*") {
                return Ok(());
            }
            if self.is_permitted(subject, actions::SANDBOX_EXEC, "*") {
                return Ok(());
            }
            return Err(EaiError::governance(format!(
                "MAC DENY: exec_command blocked under {} — use sandbox_exec or grant {} / {}",
                self.mode().as_str(),
                actions::PROCESS_EXEC,
                actions::SANDBOX_EXEC
            )));
        }

        let mut reqs = Self::requirements_for_tool(tool);
        // Task inspection is local unless `refresh` asks the cloud vendor;
        // provider checks are local unless `live` lists the vendor's models,
        // and the harness registry is local unless verified or discovered.
        let flag = |key: &str| arg.get(key).and_then(|v| v.as_bool()) == Some(true);
        if (matches!(tool, "agents_status" | "frameworks_status") && flag("refresh"))
            || (matches!(tool, "provider_doctor" | "provider_models") && flag("live"))
            || (tool == "harness_models"
                && matches!(
                    arg.get("mode").and_then(|v| v.as_str()),
                    Some("verify" | "discover" | "available")
                ))
        {
            reqs.push((actions::NETWORK_EGRESS, "*"));
        }
        // A harness run creates worktrees, runs builds and drives the susi
        // binary per task; a stdio MCP server is a child process the daemon
        // launches. Both are host program execution.
        let stdio_mcp = tool == "mcp_add"
            && !arg
                .get("command_or_url")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://"));
        if tool == "harness_run" || stdio_mcp {
            reqs.push((actions::PROCESS_EXEC, "*"));
        }
        // Adapter overrides are host configuration a later run executes.
        if matches!(tool, "adapter_configure" | "adapter_reset") {
            reqs.push((actions::FILESYSTEM_WRITE, "*"));
        }
        if tool == "adapter_configure" {
            if let Some(doc) = arg.get("adapter").filter(|v| v.is_object()) {
                reqs.extend(Self::adapter_document_requirements(
                    arg.get("kind").and_then(|v| v.as_str()),
                    doc,
                ));
            }
        }
        for (action, resource) in reqs {
            if action == actions::NETWORK_EGRESS && self.blocks_network_by_default() {
                if !self.is_permitted(subject, action, resource) {
                    return Err(EaiError::governance(format!(
                        "MAC DENY: `{tool}` needs {action} — run `susi privacy consent --egress` or grant a capability token"
                    )));
                }
            } else if !self.is_permitted(subject, action, resource) {
                return Err(EaiError::governance(format!(
                    "MAC DENY: subject `{subject}` lacks {action} on {resource} for tool `{tool}`"
                )));
            }
        }
        Ok(())
    }

    /// Resolve a workspace a socket caller names for a tool that acts on a
    /// caller's working tree (the CLI's cwd). The socket has no caller cwd,
    /// so the path must be absolute; it must be an existing directory and
    /// not a filesystem root, and `subject` must hold `filesystem.write`
    /// (`write`) or `filesystem.read` on it under this policy, so an
    /// operator confines socket workspaces by narrowing those grants.
    /// Returns the canonical path (without the Windows verbatim prefix).
    pub fn confine_workspace(&self, subject: &str, path: &str, write: bool) -> EaiResult<PathBuf> {
        let candidate = PathBuf::from(path.trim());
        if !candidate.is_absolute() {
            return Err(EaiError::config(
                "workspace must be an absolute path (the socket has no caller working directory)",
            ));
        }
        let canonical = candidate
            .canonicalize()
            .map_err(|e| EaiError::config(format!("workspace {}: {e}", candidate.display())))?;
        if !canonical.is_dir() {
            return Err(EaiError::config(format!(
                "workspace {} is not a directory",
                canonical.display()
            )));
        }
        let plain = strip_verbatim_prefix(&canonical);
        if plain.parent().is_none() {
            return Err(EaiError::config(format!(
                "workspace {} is a filesystem root",
                plain.display()
            )));
        }
        let resource = plain.to_string_lossy().replace('\\', "/");
        let action = if write {
            actions::FILESYSTEM_WRITE
        } else {
            actions::FILESYSTEM_READ
        };
        if !self.is_permitted(subject, action, &resource) {
            return Err(EaiError::governance(format!(
                "MAC DENY: subject `{subject}` lacks {action} on workspace {resource} — grant it with `privacy_grant` (action {action}, resource {resource})"
            )));
        }
        Ok(plain)
    }

    /// Resolve one file path a socket caller names for a tool to read
    /// (`write == false`) or write. The socket has no caller working
    /// directory, so the path must be absolute. The path need not exist
    /// (an output, or a state file a run creates): its nearest existing
    /// ancestor is canonicalized and the rest re-joined, and that rest may
    /// not climb (`..`). `subject` must hold `filesystem.write` (`write`)
    /// or `filesystem.read` on the resolved path, so an operator confines
    /// socket file access by narrowing those grants. Returns the resolved
    /// path (without the Windows verbatim prefix).
    pub fn confine_path(&self, subject: &str, path: &str, write: bool) -> EaiResult<PathBuf> {
        let candidate = PathBuf::from(path.trim());
        if !candidate.is_absolute() {
            return Err(EaiError::config(format!(
                "{} must be an absolute path (the socket has no caller working directory)",
                candidate.display()
            )));
        }
        let mut existing = candidate.as_path();
        let mut rest = Vec::new();
        let base = loop {
            match existing.canonicalize() {
                Ok(base) => break base,
                Err(error) => {
                    let (Some(parent), Some(name)) = (existing.parent(), existing.file_name())
                    else {
                        return Err(EaiError::config(format!(
                            "{}: {error}",
                            candidate.display()
                        )));
                    };
                    rest.push(name.to_os_string());
                    existing = parent;
                }
            }
        };
        if candidate
            .strip_prefix(existing)
            .map(|tail| {
                tail.components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            })
            .unwrap_or(true)
        {
            return Err(EaiError::config(format!(
                "{} may not climb out of an existing directory",
                candidate.display()
            )));
        }
        let mut resolved = strip_verbatim_prefix(&base);
        for name in rest.iter().rev() {
            resolved.push(name);
        }
        let resource = resolved.to_string_lossy().replace('\\', "/");
        let action = if write {
            actions::FILESYSTEM_WRITE
        } else {
            actions::FILESYSTEM_READ
        };
        if !self.is_permitted(subject, action, &resource) {
            return Err(EaiError::governance(format!(
                "MAC DENY: subject `{subject}` lacks {action} on {resource} — grant it with `privacy_grant` (action {action}, resource {resource})"
            )));
        }
        Ok(resolved)
    }

    /// Snapshot for CLI / telemetry.
    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": self.mode().as_str(),
            "mandatory_sandbox": self.mandatory_sandbox(),
            "blocks_cloud_inference": self.blocks_cloud_inference(),
            "blocks_network_by_default": self.blocks_network_by_default(),
            "grants": self.grants_for("susi"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: PrivacyMode) -> MacPolicy {
        MacPolicy::new([7u8; 32], mode, matches!(mode, PrivacyMode::LocalOnly))
    }

    #[test]
    fn socket_workspaces_are_absolute_existing_directories_the_policy_grants() {
        let p = policy(PrivacyMode::Balanced);
        let dir = std::env::temp_dir().join(format!("susi-mac-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.display().to_string();
        assert!(p.confine_workspace("susi", "relative/dir", false).is_err());
        assert!(p
            .confine_workspace("susi", &dir.join("missing").display().to_string(), false)
            .is_err());
        let root = dir.ancestors().last().unwrap().display().to_string();
        let refused = p.confine_workspace("susi", &root, false).unwrap_err();
        assert!(refused.to_string().contains("filesystem root"), "{refused}");
        // The seeded defaults grant `susi` read and write on `*`.
        let resolved = p.confine_workspace("susi", &text, true).unwrap();
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(
            resolved.canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        // A subject with no grant on the path is refused.
        let denied = p.confine_workspace("stranger", &text, false).unwrap_err();
        assert!(denied.to_string().contains("MAC DENY"), "{denied}");
        // A grant on the workspace (forward slashes, as the resource is
        // spelled) admits that subject.
        let resource = resolved.to_string_lossy().replace('\\', "/");
        p.grant("stranger", actions::FILESYSTEM_READ, &resource, None)
            .unwrap();
        assert!(p.confine_workspace("stranger", &text, false).is_ok());
        assert!(p.confine_workspace("stranger", &text, true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tokens_verify_and_expire() {
        let p = policy(PrivacyMode::Balanced);
        let t = p
            .grant("agent-a", actions::NETWORK_EGRESS, "*", Some(3600))
            .unwrap();
        assert!(p.verify(&t));
        assert!(p.is_permitted("agent-a", actions::NETWORK_EGRESS, "*"));
        let expired = p
            .grant("agent-b", actions::NETWORK_EGRESS, "*", Some(0))
            .unwrap();
        assert!(!p.is_permitted("agent-b", actions::NETWORK_EGRESS, "*"));
        assert!(expired.is_expired(now_secs()));
    }

    #[test]
    fn live_wired_readers_follow_revocation_and_refuse_a_misfiled_token() {
        let root = std::env::temp_dir().join(format!(
            "susi-mac-live-grants-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writer = policy(PrivacyMode::LocalOnly).with_state_dir(root.clone());
        let reader = policy(PrivacyMode::LocalOnly).with_state_dir(root.clone());
        let scope = format!("mcp://peers/{}/", "long-resource".repeat(30));
        writer
            .grant("worker", actions::PROCESS_EXEC, &scope, None)
            .unwrap();
        assert!(reader.is_permitted("worker", actions::PROCESS_EXEC, &scope));
        // A correctly signed record from a different binding cannot supply this one.
        let dir = root.join("mac.grants");
        let (valid, _) = grant_paths(&dir, "worker", actions::PROCESS_EXEC, &scope);
        let (wrong, _) = grant_paths(&dir, "different", actions::NETWORK_EGRESS, "*");
        std::fs::copy(valid, wrong).unwrap();
        assert!(!reader.is_permitted(
            "different",
            actions::NETWORK_EGRESS,
            "https://example.invalid"
        ));
        assert!(writer
            .revoke("worker", actions::PROCESS_EXEC, &scope)
            .unwrap());
        assert!(!reader.is_permitted("worker", actions::PROCESS_EXEC, &scope));
        assert!(!writer
            .revoke("worker", actions::PROCESS_EXEC, &scope)
            .unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocked_consent_record_does_not_publish_the_other_capability() {
        let root = std::env::temp_dir().join(format!(
            "susi-mac-failed-consent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writer = policy(PrivacyMode::LocalOnly).with_state_dir(root.clone());
        let reader = policy(PrivacyMode::LocalOnly).with_state_dir(root.clone());
        let (blocked, _) = grant_paths(
            &root.join("mac.grants"),
            "agent",
            actions::CLOUD_INFERENCE,
            "*",
        );
        std::fs::create_dir_all(blocked).unwrap();
        assert!(writer.consent_egress("agent", 60).is_err());
        assert!(!writer.is_permitted("agent", actions::NETWORK_EGRESS, "*"));
        assert!(!reader.is_permitted("agent", actions::NETWORK_EGRESS, "*"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_only_denies_network_without_consent() {
        let p = policy(PrivacyMode::LocalOnly);
        let arg = serde_json::json!({"url": "https://example.com"});
        assert!(p
            .authorize_tool("browser_automate", &arg, Path::new("."), None)
            .is_err());
        let _ = p.consent_egress("susi", 60).unwrap();
        assert!(p
            .authorize_tool("browser_automate", &arg, Path::new("."), None)
            .is_ok());
    }

    #[test]
    fn local_only_gates_every_off_host_tool() {
        let p = MacPolicy::new([7u8; 32], PrivacyMode::LocalOnly, true);
        let ws = Path::new(".");
        let none = serde_json::json!({});
        for tool in MacPolicy::EGRESS_TOOLS {
            assert!(p.authorize_tool(tool, &none, ws, None).is_err(), "{tool}");
        }
        let refresh = serde_json::json!({ "refresh": true });
        assert!(p.authorize_tool("agents_status", &none, ws, None).is_ok());
        assert!(p
            .authorize_tool("agents_status", &refresh, ws, None)
            .is_err());
        p.consent_egress("susi", 60).unwrap();
        // harness_run also executes host programs; egress consent alone
        // does not admit it (see harness_run_and_stdio_mcp_need_process_exec).
        for tool in MacPolicy::EGRESS_TOOLS {
            let verdict = p.authorize_tool(tool, &none, ws, None);
            if *tool == "harness_run" {
                assert!(verdict.is_err(), "{tool}");
            } else {
                assert!(verdict.is_ok(), "{tool}");
            }
        }
        assert!(p
            .authorize_tool("agents_status", &refresh, ws, None)
            .is_ok());
    }

    #[test]
    fn keyed_harness_reads_need_egress_under_local_only() {
        let p = MacPolicy::new([7u8; 32], PrivacyMode::LocalOnly, true);
        let ws = Path::new(".");
        let none = serde_json::json!({});
        for tool in ["harness_budget", "harness_plan"] {
            assert_eq!(
                MacPolicy::requirements_for_tool(tool),
                vec![(actions::NETWORK_EGRESS, "*")],
                "{tool}"
            );
            assert!(p.authorize_tool(tool, &none, ws, None).is_err(), "{tool}");
        }
        for mode in ["verify", "discover", "available"] {
            let arg = serde_json::json!({ "mode": mode });
            assert!(
                p.authorize_tool("harness_models", &arg, ws, None).is_err(),
                "{mode}"
            );
        }
        let registry = serde_json::json!({ "mode": "registry" });
        assert!(p
            .authorize_tool("harness_models", &registry, ws, None)
            .is_ok());
        p.consent_egress("susi", 60).unwrap();
        for tool in ["harness_budget", "harness_plan"] {
            assert!(p.authorize_tool(tool, &none, ws, None).is_ok(), "{tool}");
        }
        let available = serde_json::json!({ "mode": "available" });
        assert!(p
            .authorize_tool("harness_models", &available, ws, None)
            .is_ok());
    }

    #[test]
    fn harness_run_and_stdio_mcp_need_process_exec() {
        // Egress allowed, process.exec revoked: what `privacy consent
        // --egress` leaves under local_only, or an operator's revocation.
        let p = policy(PrivacyMode::Balanced);
        p.revoke("susi", actions::PROCESS_EXEC, "*").unwrap();
        let ws = Path::new(".");
        assert!(p.is_permitted("susi", actions::NETWORK_EGRESS, "*"));
        let denied = p
            .authorize_tool("harness_run", &serde_json::json!({}), ws, None)
            .unwrap_err();
        assert!(
            denied.to_string().contains(actions::PROCESS_EXEC),
            "{denied}"
        );
        let stdio = serde_json::json!({"name": "n", "command_or_url": "/usr/bin/evil"});
        assert!(p.authorize_tool("mcp_add", &stdio, ws, None).is_err());
        let http = serde_json::json!({"name": "n", "command_or_url": "HTTPS://mcp.example/x"});
        assert!(p.authorize_tool("mcp_add", &http, ws, None).is_ok());
        p.grant("susi", actions::PROCESS_EXEC, "*", None).unwrap();
        assert!(p
            .authorize_tool("harness_run", &serde_json::json!({}), ws, None)
            .is_ok());
        assert!(p.authorize_tool("mcp_add", &stdio, ws, None).is_ok());
    }

    #[test]
    fn adapter_overrides_need_write_and_what_the_adapter_runs() {
        let p = policy(PrivacyMode::Balanced);
        let ws = Path::new(".");
        let command = serde_json::json!({"kind": "agents", "id": "a",
            "adapter": {"kind": "command", "program": "/bin/sh", "args": []}});
        let acp = serde_json::json!({"kind": "frameworks", "id": "f",
            "adapter": {"kind": "acp", "program": "x", "args": []}});
        let a2a = serde_json::json!({"kind": "agents", "id": "r",
            "adapter": {"kind": "a2a", "url": "https://agent.example"}});
        let mcp_runner =
            serde_json::json!({"kind": "mcp", "id": "m", "adapter": {"runner": "npx"}});
        let model = serde_json::json!({"kind": "models", "id": "c", "adapter": {"model": "x"}});
        assert_eq!(
            MacPolicy::requirements_for_tool("adapter_configure"),
            vec![(actions::FILESYSTEM_READ, "*")]
        );
        for arg in [&command, &acp, &a2a, &mcp_runner, &model] {
            assert!(
                p.authorize_tool("adapter_configure", arg, ws, None).is_ok(),
                "{arg}"
            );
        }
        // Without process.exec a program-naming adapter is refused; an A2A
        // agent and a model override are not programs.
        p.revoke("susi", actions::PROCESS_EXEC, "*").unwrap();
        for arg in [&command, &acp, &mcp_runner] {
            let denied = p.authorize_tool("adapter_configure", arg, ws, None);
            assert!(denied.is_err(), "{arg}");
        }
        assert!(p
            .authorize_tool("adapter_configure", &a2a, ws, None)
            .is_ok());
        assert!(p
            .authorize_tool("adapter_configure", &model, ws, None)
            .is_ok());
        // Without egress the A2A agent is refused too.
        p.revoke("susi", actions::NETWORK_EGRESS, "*").unwrap();
        assert!(p
            .authorize_tool("adapter_configure", &a2a, ws, None)
            .is_err());
        // Without filesystem.write no override is stored or dropped.
        p.revoke("susi", actions::FILESYSTEM_WRITE, "*").unwrap();
        let reset = serde_json::json!({"kind": "models", "id": "c"});
        assert!(p.authorize_tool("adapter_reset", &reset, ws, None).is_err());
        assert!(p
            .authorize_tool("adapter_configure", &model, ws, None)
            .is_err());
    }

    #[test]
    fn socket_file_paths_are_absolute_and_confined_by_the_grants() {
        let p = policy(PrivacyMode::LocalOnly);
        let dir = std::env::temp_dir().join(format!("susi-mac-path-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("susi-mac-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let granted = strip_verbatim_prefix(&dir.canonicalize().unwrap())
            .to_string_lossy()
            .replace('\\', "/");
        p.grant("worker", actions::FILESYSTEM_WRITE, &granted, None)
            .unwrap();
        assert!(p.confine_path("worker", "rel/out.json", true).is_err());
        // A new output (and a new directory under it) inside the grant.
        let new_leaf = dir.join("sub").join("out.json");
        let resolved = p
            .confine_path("worker", &new_leaf.display().to_string(), true)
            .unwrap();
        assert!(resolved.ends_with(Path::new("sub").join("out.json")));
        // Outside the grant, and a read the grant does not cover.
        let denied = p
            .confine_path(
                "worker",
                &outside.join("x.json").display().to_string(),
                true,
            )
            .unwrap_err();
        assert!(denied.to_string().contains("MAC DENY"), "{denied}");
        assert!(p
            .confine_path("worker", &new_leaf.display().to_string(), false)
            .is_err());
        // Climbing out of the granted directory resolves outside it.
        let climb = format!(
            "{}/../{}/x.json",
            dir.display(),
            outside.file_name().unwrap().to_string_lossy()
        );
        assert!(p.confine_path("worker", &climb, true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn local_only_blocks_host_exec_without_grant() {
        let p = policy(PrivacyMode::LocalOnly);
        // sandbox.exec is seeded — authorize allows redirect path
        assert!(p
            .authorize_tool(
                "exec_command",
                &serde_json::json!("ls"),
                Path::new("."),
                None
            )
            .is_ok());
        assert!(p
            .authorize_tool(
                "sandbox_exec",
                &serde_json::json!({"cmd": "echo hi"}),
                Path::new("."),
                None
            )
            .is_ok());
        assert!(!p.is_permitted("susi", actions::PROCESS_EXEC, "*"));
    }

    #[test]
    fn balanced_allows_read_write_exec() {
        let p = policy(PrivacyMode::Balanced);
        assert!(p
            .authorize_tool("read_file", &serde_json::json!({}), Path::new("."), None)
            .is_ok());
        assert!(p
            .authorize_tool(
                "exec_command",
                &serde_json::json!("cargo test"),
                Path::new("."),
                None
            )
            .is_ok());
        // Balanced seeds signed network.egress for host continuity.
        assert!(p
            .authorize_tool(
                "browser_automate",
                &serde_json::json!({}),
                Path::new("."),
                None
            )
            .is_ok());
    }

    #[test]
    fn switching_to_local_only_revokes_cloud_grants() {
        let p = policy(PrivacyMode::Balanced);
        assert!(p.is_permitted("susi", actions::CLOUD_INFERENCE, "*"));
        p.set_mode(PrivacyMode::LocalOnly).unwrap();
        assert!(p.blocks_cloud_inference());
        assert!(!p.is_permitted("susi", actions::NETWORK_EGRESS, "*"));
        assert!(!p.is_permitted("susi", actions::PROCESS_EXEC, "*"));
        assert!(p.mandatory_sandbox());
    }

    #[test]
    fn damaged_sticky_mode_fails_closed() {
        let dir = std::env::temp_dir().join(format!(
            "susi-mac-mode-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_sticky_mode(&dir), None);
        for (text, want) in [
            ("open", PrivacyMode::Open),
            ("balanced\n", PrivacyMode::Balanced),
            ("local_only", PrivacyMode::LocalOnly),
            ("", PrivacyMode::LocalOnly),
            ("loc", PrivacyMode::LocalOnly),
        ] {
            std::fs::write(dir.join("privacy_mode"), text).unwrap();
            assert_eq!(read_sticky_mode(&dir), Some(want), "{text:?}");
        }
        let policy = policy(PrivacyMode::Balanced).with_state_dir(dir.clone());
        policy.set_mode(PrivacyMode::Open).unwrap();
        assert_eq!(read_sticky_mode(&dir), Some(PrivacyMode::Open));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scoped_grants_stop_at_a_path_or_host_boundary() {
        let p = policy(PrivacyMode::LocalOnly);
        p.grant("cell-a", actions::FILESYSTEM_READ, "/srv/ws", None)
            .unwrap();
        assert!(p.is_permitted("cell-a", actions::FILESYSTEM_READ, "/srv/ws"));
        assert!(p.is_permitted("cell-a", actions::FILESYSTEM_READ, "/srv/ws/src/lib.rs"));
        assert!(!p.is_permitted("cell-a", actions::FILESYSTEM_READ, "/srv/ws2"));
        assert!(!p.is_permitted("cell-a", actions::FILESYSTEM_READ, "/srv/ws-secrets/key"));
        p.grant(
            "cell-a",
            actions::NETWORK_EGRESS,
            "https://api.example.com",
            None,
        )
        .unwrap();
        assert!(p.is_permitted(
            "cell-a",
            actions::NETWORK_EGRESS,
            "https://api.example.com/v1"
        ));
        assert!(!p.is_permitted(
            "cell-a",
            actions::NETWORK_EGRESS,
            "https://api.example.com.evil.net/x"
        ));
        // An explicit trailing `*` is a glob over its prefix.
        p.grant("cell-a", actions::TRANSMIT, "app://*", None)
            .unwrap();
        assert!(p.is_permitted("cell-a", actions::TRANSMIT, "app://agent-b"));
        assert!(!p.is_permitted("cell-a", actions::TRANSMIT, "tcp://agent-b"));
    }

    #[test]
    fn ipc_send_is_authorized_by_the_default_transmit_grant() {
        for mode in [
            PrivacyMode::Balanced,
            PrivacyMode::LocalOnly,
            PrivacyMode::Open,
        ] {
            let p = policy(mode);
            let ws = std::env::temp_dir();
            let arg = serde_json::json!({"from": "agent-a", "to": "agent-b", "payload": "x"});
            assert!(
                p.authorize_tool("ipc_send", &arg, &ws, None).is_ok(),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn only_host_and_lan_urls_stay_local() {
        for local in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:8000/v1",
            "http://192.168.1.20:8000/v1",
            "http://10.0.0.5/v1",
            "http://[::1]:8080",
            "http://user:pw@gpu-box.local:8000/v1",
        ] {
            assert!(url_stays_local(local), "{local}");
        }
        for remote in [
            "https://api.openai.com/v1",
            "http://203.0.113.9:8000/v1",
            "https://huggingface.co/x/resolve/main/m.gguf",
            "http://localhost.evil.com/v1",
            "http://evil.com/?h=localhost",
            "not a url",
        ] {
            assert!(!url_stays_local(remote), "{remote}");
        }
    }

    #[test]
    fn web_search_tools_need_egress_but_semantic_search_stays_local() {
        for tool in ["brave_search", "google_search", "web_search"] {
            assert_eq!(
                MacPolicy::requirements_for_tool(tool),
                vec![(actions::NETWORK_EGRESS, "*")],
                "{tool}"
            );
        }
        assert_eq!(
            MacPolicy::requirements_for_tool("semantic_search"),
            vec![(actions::FILESYSTEM_READ, "*")]
        );
    }

    #[test]
    fn a_signed_binding_with_a_delimiter_cannot_be_repartitioned() {
        let policy = policy(PrivacyMode::LocalOnly);
        let mut token = policy
            .grant("mission|worker", "process.exec", "*", None)
            .unwrap();
        assert!(policy.verify(&token));
        token.subject = "mission".into();
        token.action = "worker|process.exec".into();
        assert!(!policy.verify(&token));
        let ambiguous = format!("mission|worker|process.exec|*|{}|none", token.issued_at);
        token.signature = hex::encode(hmac_sha256(&policy.key, ambiguous.as_bytes()));
        assert!(!policy.verify(&token));
        assert_ne!(
            grant_key("mission|worker", "process.exec", "*"),
            grant_key("mission", "worker|process.exec", "*")
        );
    }

    #[test]
    fn tampered_token_rejected() {
        let p = policy(PrivacyMode::Open);
        let mut t = p.grant("x", actions::FILESYSTEM_READ, "*", None).unwrap();
        t.signature = "00".repeat(32);
        assert!(!p.verify(&t));
    }
}
