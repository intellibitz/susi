//! Mandatory Access Control (MAC) for agent tool I/O and egress.
//!
//! Cryptographic capability tokens (HMAC-SHA256) bound to subject / action /
//! resource / expiry. Composition root loads the host key and privacy mode;
//! every tool dispatch must pass [`MacPolicy::authorize_tool`].

use crate::susi_error::{EaiError, EaiResult};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

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
            "{}|{}|{}|{}|{}",
            self.subject,
            self.action,
            self.resource,
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
    format!("{subject}|{action}|{resource}")
}

/// Process-wide MAC enforcer.
pub struct MacPolicy {
    key: [u8; 32],
    grants: DashMap<String, CapabilityToken>,
    mode: RwLock<PrivacyMode>,
    mandatory_sandbox: AtomicBool,
    /// When true, every elevated check requires a verified token (always on).
    enforce: AtomicBool,
    /// Shared substrate state dir (set on the wired path): grants persist to
    /// `<dir>/mac.grants/` and privacy mode reads/writes `<dir>/privacy_mode`
    /// so vendored `susi_core` copies share policy state with this process.
    state_dir: Option<PathBuf>,
}

static POLICY: OnceLock<MacPolicy> = OnceLock::new();

fn enc(key: &str) -> String {
    crate::susi_core::plane_bus_ipc::enc(key)
}

/// Load the shared substrate HMAC key, creating it (0600) on first boot.
/// All wired copies — daemon's and vendored — must sign/verify with it, so
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
            grants: DashMap::new(),
            mode: RwLock::new(mode),
            mandatory_sandbox: AtomicBool::new(mandatory_sandbox),
            enforce: AtomicBool::new(true),
            state_dir: None,
        };
        p.seed_defaults();
        p
    }

    /// Enable shared-state persistence: grants mirror to `<dir>/mac.grants/`
    /// and mode reads/writes `<dir>/privacy_mode`.
    fn with_state_dir(mut self, dir: PathBuf) -> Self {
        let grants = dir.join("mac.grants");
        let _ = std::fs::create_dir_all(&grants);
        // Replay persisted grants so this copy sees tokens issued elsewhere.
        if let Ok(rd) = std::fs::read_dir(&grants) {
            for e in rd.flatten() {
                if let Some(token) = std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|s| serde_json::from_str::<CapabilityToken>(&s).ok())
                {
                    self.grants.insert(
                        grant_key(&token.subject, &token.action, &token.resource),
                        token,
                    );
                }
            }
        }
        self.state_dir = Some(dir);
        self
    }

    /// Initialize the global policy (first call wins). Wired via the
    /// substrate state dir so vendored copies share grants and mode.
    pub fn init_global(key: [u8; 32], mode: PrivacyMode, mandatory_sandbox: bool) -> &'static Self {
        POLICY.get_or_init(|| {
            Self::new(key, mode, mandatory_sandbox)
                .with_state_dir(crate::susi_paths::SusiDirs::substrate_home())
        })
    }

    /// The wired constructor vendored copies use for `global()`: shared key
    /// file, sticky mode, persisted grants — all under `substrate_home`.
    pub fn wired() -> Self {
        let dir = crate::susi_paths::SusiDirs::substrate_home();
        let key = match load_or_create_key(&dir) {
            Ok(key) => key,
            Err(error) => {
                // No shared key: grants this copy signs cannot be verified
                // by the rest of the substrate, so fail closed rather than
                // run a permissive policy on a private key.
                tracing::error!(%error, dir = %dir.display(), "substrate MAC key unavailable; enforcing local_only");
                return Self::new(process_local_key(), PrivacyMode::LocalOnly, true);
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

    fn persist_grant(&self, token: &CapabilityToken) {
        let Some(dir) = self.grants_dir() else { return };
        let path = dir.join(enc(&grant_key(
            &token.subject,
            &token.action,
            &token.resource,
        )));
        if let Ok(bytes) = serde_json::to_vec(token) {
            let _ = crate::susi_config::atomic_write_bytes(&path, &bytes);
        }
    }

    /// File-backed grant lookup: exact key, wildcard `*`, then a prefix scan.
    /// Keeps tokens issued by other wired copies visible to this one.
    fn persisted_grant(
        &self,
        subject: &str,
        action: &str,
        resource: &str,
    ) -> Option<CapabilityToken> {
        let dir = self.grants_dir()?;
        let read = |key: String| -> Option<CapabilityToken> {
            std::fs::read_to_string(dir.join(enc(&key)))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        };
        let now = now_secs();
        let live = |t: &CapabilityToken| self.verify(t) && !t.is_expired(now);
        if let Some(t) = read(grant_key(subject, action, resource)) {
            if live(&t) {
                return Some(t);
            }
        }
        if resource != "*" {
            if let Some(t) = read(grant_key(subject, action, "*")) {
                if live(&t) {
                    return Some(t);
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let Ok(text) = std::fs::read_to_string(e.path()) else {
                    continue;
                };
                let Ok(t) = serde_json::from_str::<CapabilityToken>(&text) else {
                    continue;
                };
                if t.subject == subject
                    && t.action == action
                    && resource.starts_with(&t.resource)
                    && live(&t)
                {
                    return Some(t);
                }
            }
        }
        None
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
        self.revoke("susi", actions::NETWORK_EGRESS, "*");
        self.revoke("susi", actions::CLOUD_INFERENCE, "*");
        self.revoke("susi", actions::PROCESS_EXEC, "*");
        self.mandatory_sandbox
            .store(matches!(mode, PrivacyMode::LocalOnly), Ordering::Relaxed);
        self.seed_defaults();
        Ok(())
    }

    /// Whether this policy shares its posture through the sticky file
    /// (`set_mode` then persists it); unwired copies keep it in memory.
    pub fn persists_mode(&self) -> bool {
        self.state_dir.is_some()
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

    /// Issue and store a capability token.
    pub fn grant(
        &self,
        subject: &str,
        action: &str,
        resource: &str,
        ttl_secs: Option<u64>,
    ) -> CapabilityToken {
        let issued = now_secs();
        let mut token = CapabilityToken {
            subject: subject.into(),
            action: action.into(),
            resource: resource.into(),
            issued_at: issued,
            expires_at: ttl_secs.map(|t| issued + t),
            signature: String::new(),
        };
        token.signature = self.sign(&token);
        self.grants
            .insert(grant_key(subject, action, resource), token.clone());
        self.persist_grant(&token);
        token
    }

    pub fn revoke(&self, subject: &str, action: &str, resource: &str) -> bool {
        let key = grant_key(subject, action, resource);
        if let Some(dir) = self.grants_dir() {
            let _ = std::fs::remove_file(dir.join(enc(&key)));
        }
        self.grants.remove(&key).is_some()
    }

    pub fn grants_for(&self, subject: &str) -> Vec<CapabilityToken> {
        let mut seen: std::collections::HashMap<String, CapabilityToken> = self
            .grants
            .iter()
            .filter(|e| e.value().subject == subject)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        if let Some(dir) = self.grants_dir() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let Ok(text) = std::fs::read_to_string(e.path()) else {
                        continue;
                    };
                    let Ok(t) = serde_json::from_str::<CapabilityToken>(&text) else {
                        continue;
                    };
                    if t.subject == subject {
                        seen.entry(grant_key(&t.subject, &t.action, &t.resource))
                            .or_insert(t);
                    }
                }
            }
        }
        seen.into_values().collect()
    }

    /// Explicit operator consent for network egress (and optionally cloud).
    pub fn consent_egress(&self, subject: &str, ttl_secs: u64) -> Vec<CapabilityToken> {
        let mut out = vec![self.grant(subject, actions::NETWORK_EGRESS, "*", Some(ttl_secs))];
        if matches!(self.mode(), PrivacyMode::LocalOnly) {
            out.push(self.grant(subject, actions::CLOUD_INFERENCE, "*", Some(ttl_secs)));
        }
        out
    }

    pub fn is_permitted(&self, subject: &str, action: &str, resource: &str) -> bool {
        if !self.enforce.load(Ordering::Relaxed) {
            return true;
        }
        let now = now_secs();
        // Exact match
        if let Some(t) = self.grants.get(&grant_key(subject, action, resource)) {
            if self.verify(t.value()) && !t.is_expired(now) {
                return true;
            }
        }
        // Wildcard resource grant
        if resource != "*" {
            if let Some(t) = self.grants.get(&grant_key(subject, action, "*")) {
                if self.verify(t.value()) && !t.is_expired(now) {
                    return true;
                }
            }
        }
        // Prefix resource: grant resource is a prefix of requested
        for entry in self.grants.iter() {
            let t = entry.value();
            if t.subject == subject
                && t.action == action
                && resource.starts_with(&t.resource)
                && self.verify(t)
                && !t.is_expired(now)
            {
                return true;
            }
        }
        // Grants persisted by other wired copies (vendored susi_core).
        self.persisted_grant(subject, action, resource).is_some()
    }

    fn seed_defaults(&self) {
        let subject = "susi";
        // Always allow local read of workspace-scoped data.
        let _ = self.grant(subject, actions::FILESYSTEM_READ, "*", None);
        let _ = self.grant(subject, actions::FILESYSTEM_WRITE, "*", None);
        let _ = self.grant(subject, actions::SANDBOX_EXEC, "*", None);
        let _ = self.grant(subject, actions::TRANSMIT, "app://*", None);

        match self.mode() {
            PrivacyMode::Open => {
                let _ = self.grant(subject, actions::PROCESS_EXEC, "*", None);
                let _ = self.grant(subject, actions::NETWORK_EGRESS, "*", None);
                let _ = self.grant(subject, actions::CLOUD_INFERENCE, "*", None);
            }
            PrivacyMode::Balanced => {
                let _ = self.grant(subject, actions::PROCESS_EXEC, "*", None);
                // Signed default egress for host substrate so zero-config MCP
                // keeps working; revoke or switch to local_only to lock down.
                let _ = self.grant(subject, actions::NETWORK_EGRESS, "*", None);
                let _ = self.grant(subject, actions::CLOUD_INFERENCE, "*", None);
            }
            PrivacyMode::LocalOnly => {
                // No host process.exec by default — sandbox only.
                // No network / cloud without consent_egress.
            }
        }
    }

    /// Tools that send data off-host but whose names match none of the
    /// substring rules below; without this list they were classified as
    /// local reads and bypassed the egress gate under `local_only`.
    const EGRESS_TOOLS: &'static [&'static str] = &[
        "a2a_delegate",      // task to a remote A2A peer
        "agents_send",       // message to a cloud agent task
        "power_reason",      // delegation to remote MCP servers
        "meta_scout_agents", // capability listing from connected remotes
    ];

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
            return vec![(actions::TRANSMIT, "*")];
        }
        if t == "reason" || t == "susi_solve" || t.starts_with("agents_") {
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
        // Task inspection is local unless `refresh` asks the cloud vendor.
        if matches!(tool, "agents_status" | "frameworks_status")
            && arg.get("refresh").and_then(|v| v.as_bool()) == Some(true)
        {
            reqs.push((actions::NETWORK_EGRESS, "*"));
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
    fn tokens_verify_and_expire() {
        let p = policy(PrivacyMode::Balanced);
        let t = p.grant("agent-a", actions::NETWORK_EGRESS, "*", Some(3600));
        assert!(p.verify(&t));
        assert!(p.is_permitted("agent-a", actions::NETWORK_EGRESS, "*"));
        let expired = p.grant("agent-b", actions::NETWORK_EGRESS, "*", Some(0));
        assert!(!p.is_permitted("agent-b", actions::NETWORK_EGRESS, "*"));
        assert!(expired.is_expired(now_secs()));
    }

    #[test]
    fn local_only_denies_network_without_consent() {
        let p = policy(PrivacyMode::LocalOnly);
        let arg = serde_json::json!({"url": "https://example.com"});
        assert!(p
            .authorize_tool("browser_automate", &arg, Path::new("."), None)
            .is_err());
        let _ = p.consent_egress("susi", 60);
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
        p.consent_egress("susi", 60);
        for tool in MacPolicy::EGRESS_TOOLS {
            assert!(p.authorize_tool(tool, &none, ws, None).is_ok(), "{tool}");
        }
        assert!(p
            .authorize_tool("agents_status", &refresh, ws, None)
            .is_ok());
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
    fn tampered_token_rejected() {
        let p = policy(PrivacyMode::Open);
        let mut t = p.grant("x", actions::FILESYSTEM_READ, "*", None);
        t.signature = "00".repeat(32);
        assert!(!p.verify(&t));
    }
}
