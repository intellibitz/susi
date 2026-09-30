//! Bounded, redacted availability evidence from actual inference outcomes.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MAX_RECORDS: usize = 1024;
const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Availability {
    Unknown,
    Working,
    InvalidCredential,
    InsufficientCredit,
    QuotaExhausted,
    RateLimited,
    Unavailable,
    AccessDenied,
    UnsupportedRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Scope {
    Credential,
    Account,
    Model,
    Region,
}

/// Only opaque hashes enter persistent state; raw keys and endpoint query
/// strings are never serialized or included in Debug output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Target {
    provider: String,
    credential: String,
    account: String,
    model: String,
    region: String,
}

fn fingerprint(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    hex::encode(digest.finalize())
}

impl Target {
    /// Account/region identity must come from supported provider metadata or
    /// operator attestation. Unknown accounts are scoped to the credential,
    /// never guessed from two keys belonging to the same vendor.
    pub fn new(
        endpoint: &str,
        key: &str,
        model: &str,
        account: Option<&str>,
        region: &str,
    ) -> Self {
        let provider = fingerprint(&[endpoint]);
        let credential = fingerprint(&[endpoint, key]);
        Self {
            account: account.map_or_else(|| credential.clone(), |a| fingerprint(&[endpoint, a])),
            model: fingerprint(&[model]),
            region: fingerprint(&[region]),
            provider,
            credential,
        }
    }

    fn matches(&self, other: &Self, scope: Scope) -> bool {
        self.provider == other.provider
            && match scope {
                Scope::Credential => self.credential == other.credential,
                Scope::Account => self.account == other.account,
                Scope::Model => {
                    self.credential == other.credential
                        && self.model == other.model
                        && self.region == other.region
                }
                Scope::Region => self.region == other.region,
            }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub target: Target,
    pub state: Availability,
    pub scope: Scope,
    pub observed_at: u64,
    pub expires_at: u64,
    /// Fixed provenance, rather than provider-controlled messages or secrets.
    pub source: EvidenceSource,
    #[serde(default)]
    pub quota: Option<crate::cloud_quota::Quota>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum EvidenceSource {
    InferenceResponse,
    KeyMetadata,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Eligibility {
    records: Vec<Observation>,
}

impl Eligibility {
    pub fn observe(&mut self, observation: Observation) {
        if self.records.iter().any(|r| {
            observation.target.matches(&r.target, observation.scope)
                && r.observed_at > observation.observed_at
        }) {
            return;
        }
        self.records.retain(|r| {
            !(r.target.matches(&observation.target, r.scope)
                && (r.scope == observation.scope
                    || (observation.state == Availability::Working
                        && r.observed_at <= observation.observed_at)))
        });
        self.records.push(observation);
        self.records.sort_by_key(|r| r.observed_at);
        if self.records.len() > MAX_RECORDS {
            self.records.drain(..self.records.len() - MAX_RECORDS);
        }
    }

    /// Expired evidence and observations from a future clock are unknown.
    /// Broad current blocks override a model's older success observation.
    pub fn resolve_account(&self, mut target: Target, now: u64) -> Target {
        if let Some(observation) = self.records.iter().rev().find(|r| {
            r.observed_at <= now
                && now < r.expires_at
                && r.target.matches(&target, Scope::Credential)
        }) {
            target.account = observation.target.account.clone();
        }
        target
    }

    pub fn state(&self, target: &Target, now: u64) -> Availability {
        let applicable: Vec<_> = self
            .records
            .iter()
            .filter(|r| {
                r.observed_at <= now && now < r.expires_at && r.target.matches(target, r.scope)
            })
            .collect();
        if applicable.iter().any(|r| {
            r.quota
                .as_ref()
                .and_then(|q| q.blocked_until(now))
                .is_some()
        }) {
            return Availability::RateLimited;
        }
        if let Some(block) = applicable
            .iter()
            .rev()
            .find(|r| r.state != Availability::Working && r.state != Availability::Unknown)
        {
            return block.state;
        }
        if applicable.iter().any(|r| r.state == Availability::Working) {
            Availability::Working
        } else {
            Availability::Unknown
        }
    }

    pub fn load(path: &Path) -> EaiResult<Self> {
        match std::fs::File::open(path) {
            Ok(file) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| EaiError::io(e.to_string()))?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(EaiError::config("cloud availability file exceeds limit"));
                }
                let state: Self = serde_json::from_slice(&bytes)
                    .map_err(|_| EaiError::config("invalid cloud availability state"))?;
                if state.records.len() > MAX_RECORDS {
                    return Err(EaiError::config("too many cloud availability records"));
                }
                Ok(state)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }
}

pub fn state_path() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("cloud-availability.json")
}

/// Record while holding the shared cross-process lock; atomically publish a
/// complete snapshot. Corruption is an error, never an empty overwrite.
pub fn record(path: &Path, observation: Observation) -> EaiResult<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let _guard = susi_config::file_lock::FileLock::acquire(dir, "cloud-availability")
        .ok_or_else(|| EaiError::io("cloud availability lock unavailable"))?;
    let mut state = Eligibility::load(path)?;
    state.observe(observation);
    let bytes = serde_json::to_vec(&state)
        .map_err(|_| EaiError::config("cannot encode cloud availability"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(EaiError::config(
            "cloud availability snapshot exceeds limit",
        ));
    }
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, bytes).map_err(|e| EaiError::io(e.to_string()))?;
    std::fs::rename(staged, path).map_err(|e| EaiError::io(e.to_string()))
}

/// Provider error evidence is interpreted only here, in the vendor crate.
/// Unknown errors never imply a valid key, working model, or zero balance.
pub fn classify(error: &str) -> (Availability, Scope) {
    let lower = error.to_ascii_lowercase();
    if (lower.contains("api_key") && lower.contains("not set"))
        || lower.starts_with("http 401:")
        || lower.contains("invalid_api_key")
        || lower.contains("authentication_error")
    {
        (Availability::InvalidCredential, Scope::Credential)
    } else if lower.starts_with("http 402:")
        || lower.contains("insufficient balance")
        || lower.contains("credit balance is too low")
        || lower.contains("insufficient_credit")
    {
        (Availability::InsufficientCredit, Scope::Account)
    } else if lower.contains("insufficient_quota")
        || lower.contains("quota_exceeded")
        || lower.contains("resource_exhausted")
    {
        (Availability::QuotaExhausted, Scope::Account)
    } else if lower.starts_with("http 429:") {
        (Availability::RateLimited, Scope::Credential)
    } else if lower.starts_with("http 403:") || lower.starts_with("http 404:") {
        (Availability::AccessDenied, Scope::Model)
    } else if lower.starts_with("http 400:") || lower.starts_with("http 422:") {
        // A bad payload says nothing about the next request's availability.
        (Availability::UnsupportedRequest, Scope::Model)
    } else {
        (Availability::Unavailable, Scope::Model)
    }
}

pub fn inference_observation(target: Target, outcome: Result<(), &str>, now: u64) -> Observation {
    let (state, scope) = outcome.map_or_else(classify, |()| (Availability::Working, Scope::Model));
    let ttl = match state {
        Availability::Working => 300,
        Availability::InvalidCredential
        | Availability::InsufficientCredit
        | Availability::QuotaExhausted => 3600,
        Availability::RateLimited | Availability::Unavailable | Availability::AccessDenied => 60,
        Availability::UnsupportedRequest | Availability::Unknown => 0,
    };
    Observation {
        target,
        state,
        scope,
        observed_at: now,
        expires_at: now.saturating_add(ttl),
        source: EvidenceSource::InferenceResponse,
        quota: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn target(key: &str, model: &str) -> Target {
        Target::new(
            "https://provider.invalid",
            key,
            model,
            Some("shared-account"),
            "eu",
        )
    }

    #[test]
    fn cloud_eligibility_state_classifies_failure_fixtures() {
        for (error, expected) in [
            ("HTTP 401: invalid key", Availability::InvalidCredential),
            (
                "HTTP 402: insufficient balance",
                Availability::InsufficientCredit,
            ),
            ("HTTP 429: insufficient_quota", Availability::QuotaExhausted),
            ("HTTP 429: retry later", Availability::RateLimited),
            ("HTTP 503: outage", Availability::Unavailable),
            ("HTTP 403: model forbidden", Availability::AccessDenied),
            ("HTTP 400: invalid input", Availability::UnsupportedRequest),
        ] {
            assert_eq!(classify(error).0, expected);
        }
    }

    #[test]
    fn cloud_eligibility_state_scopes_shared_accounts_and_models() {
        let mut state = Eligibility::default();
        let a = target("key-a", "model-a");
        let b = target("key-b", "model-b");
        let other_model = target("key-a", "model-b");
        state.observe(inference_observation(a.clone(), Ok(()), 10));
        state.observe(inference_observation(
            other_model.clone(),
            Err("HTTP 404: missing model"),
            11,
        ));
        assert_eq!(state.state(&a, 12), Availability::Working);
        assert_eq!(state.state(&other_model, 12), Availability::AccessDenied);
        assert_eq!(state.state(&b, 12), Availability::Unknown);
        state.observe(inference_observation(
            a.clone(),
            Err("HTTP 402: no credit"),
            13,
        ));
        assert_eq!(state.state(&b, 14), Availability::InsufficientCredit);
        let unrelated = Target::new("https://other.invalid", "key-c", "model-a", None, "eu");
        assert_eq!(state.state(&unrelated, 14), Availability::Unknown);
        state.observe(inference_observation(a.clone(), Ok(()), 15));
        assert_eq!(state.state(&a, 16), Availability::Working);
        // Success for A does not manufacture inference success for B.
        assert_eq!(state.state(&b, 16), Availability::Unknown);
    }

    #[test]
    fn cloud_eligibility_state_expiry_restart_redaction_and_corruption() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", dir.path())
            .set("XDG_DATA_HOME", dir.path().join("data"))
            .set("XDG_CONFIG_HOME", dir.path().join("config"));
        let path = dir.path().join("availability.json");
        let a = target("secret-canary-key", "model-a");
        record(&path, inference_observation(a.clone(), Ok(()), 100)).unwrap();
        let state = Eligibility::load(&path).unwrap();
        assert_eq!(state.state(&a, 99), Availability::Unknown);
        assert_eq!(state.state(&a, 101), Availability::Working);
        assert_eq!(state.state(&a, 400), Availability::Unknown);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("secret-canary-key"));
        assert!(!format!("{a:?}").contains("secret-canary-key"));
        std::fs::write(&path, b"broken").unwrap();
        assert!(record(&path, inference_observation(a, Ok(()), 101)).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
    }

    #[test]
    fn cloud_eligibility_state_concurrent_records_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("availability.json");
        std::thread::scope(|scope| {
            for n in 0..8 {
                let path = &path;
                scope.spawn(move || {
                    record(
                        path,
                        inference_observation(target(&format!("key-{n}"), "model"), Ok(()), 10),
                    )
                    .unwrap()
                });
            }
        });
        let state = Eligibility::load(&path).unwrap();
        for n in 0..8 {
            assert_eq!(
                state.state(&target(&format!("key-{n}"), "model"), 11),
                Availability::Working
            );
        }
    }

    proptest! {
        #[test]
        fn cloud_eligibility_state_stale_events_never_override_newer(events in prop::collection::vec((0u64..1000, any::<bool>()), 1..80)) {
            let a = target("key", "model");
            let mut state = Eligibility::default();
            let mut latest = 0;
            for (time, working) in events {
                let outcome = if working { Ok(()) } else { Err("HTTP 404: denied") };
                state.observe(inference_observation(a.clone(), outcome, time));
                latest = latest.max(time);
                assert!(state.records.iter().all(|r| r.observed_at == latest));
            }
        }
    }
}
