//! Per-(credential, account, model, region) cloud eligibility state.
//!
//! Provider failures are scoped to the narrowest dimension the evidence
//! supports: a bad key blocks only that credential, an exhausted quota blocks
//! the whole account (every key sharing it), a denied model blocks only that
//! model, and a regional outage blocks only that region. State is persisted
//! bounded with observation time, reason, expiry and provenance; raw keys are
//! never stored — only a truncated SHA-256 fingerprint — and reasons pass
//! through the secret masker.
//!
//! "Usable" is earned: it requires a fresh observation of *successful
//! authenticated inference* for that credential+model. A configured key, a
//! catalog entry or a `/models` listing alone never mark anything usable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use susi_error::redact;

/// Coarse availability of a credential+model pair, from provider evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityKind {
    /// A fresh successful authenticated inference was recorded.
    Usable,
    /// No usable observation, or the last one expired.
    Unknown,
    /// The credential was rejected (401, or 403 with no other signal).
    InvalidCredential,
    /// Billing/credit exhausted for the account.
    InsufficientCredit,
    /// The account's quota window is exhausted until reset.
    QuotaExhausted,
    /// Transient throttling (429 without quota evidence).
    RateLimited,
    /// Provider-side outage or unreachable service.
    ServiceUnavailable,
    /// This credential may authenticate but may not use this model.
    AccessDenied,
    /// The request shape/endpoint is unsupported for this model.
    UnsupportedRequest,
}

/// The dimension a failure applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeLevel {
    /// One API key.
    Credential,
    /// Every key billed to one account.
    Account,
    /// One model (across credentials on the provider).
    Model,
    /// One provider region (or the provider as a whole when unknown).
    Region,
}

/// Where an observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// An actual inference request's result — the only proof of `Usable`.
    Inference,
    /// A management/health probe (`/models` listing, key check).
    Probe,
    /// Operator-supplied or imported state.
    Manual,
}

/// One scoped, redacted, expiring availability observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub kind: EligibilityKind,
    pub level: ScopeLevel,
    /// Endpoint/vendor name (e.g. `openai`, `groq`) — an identifier, not a secret.
    pub provider: String,
    /// `credential_fingerprint` of the key — the raw key is never stored.
    /// `"*"` marks an observation applying to every credential.
    pub credential: String,
    /// Billing account id when provider evidence exposes one; `""` unknown.
    pub account: String,
    /// Model id the observation applies to; `""` when unrelated.
    pub model: String,
    /// Provider region when known; `""` unknown.
    pub region: String,
    /// Short redacted human-readable reason.
    pub reason: String,
    pub observed_unix: u64,
    /// When the observation stops applying (`None` = sticky until evicted).
    pub expires_unix: Option<u64>,
    pub provenance: Provenance,
}

/// The result a caller got back from one inference attempt.
#[derive(Debug, Clone)]
pub enum InferenceResult {
    /// Authenticated inference completed.
    Success,
    /// Inference failed; carries whatever provider evidence exists.
    Failed {
        /// HTTP status when the failure came with one.
        status: Option<u16>,
        /// A short body/headers snippet (masked before it is stored).
        body_snippet: String,
        /// `Retry-After` seconds when the provider supplied them.
        retry_after_secs: Option<u64>,
    },
}

/// One (credential, model) pair on a provider — the resolution target for
/// [`EligibilityStore::record_inference`] and [`EligibilityStore::resolve`].
/// `api_key` is raw key material on the stack only; it is fingerprinted,
/// never stored.
#[derive(Debug, Clone, Copy)]
pub struct Subject<'a> {
    /// Endpoint/vendor name (e.g. `openai`, `groq`).
    pub provider: &'a str,
    /// The credential used for the attempt (may be empty for keyless endpoints).
    pub api_key: &'a str,
    /// Billing account id when known/configured; keys on one account share
    /// account-scoped limits.
    pub account: Option<&'a str>,
    /// Provider region when known.
    pub region: Option<&'a str>,
    /// Model id the request targets.
    pub model: &'a str,
}

/// Resolved eligibility for one (credential, model) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub kind: EligibilityKind,
    pub reason: String,
    pub observed_unix: u64,
}

impl Verdict {
    /// A usable verdict only ever means proven inference success.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.kind == EligibilityKind::Usable
    }
}

const REASON_CAP: usize = 240;
const DEFAULT_CAP: usize = 4096;
/// A success observation stays fresh for an hour; after that the pair is
/// `Unknown` again until inference proves it once more.
const USABLE_TTL_SECS: u64 = 3600;
const RATE_LIMIT_TTL_SECS: u64 = 300;
const UNAVAILABLE_TTL_SECS: u64 = 900;
const QUOTA_TTL_SECS: u64 = 86_400;

/// Truncated SHA-256 fingerprint of a credential — stable, comparable, and
/// not reversible into the key. Empty key material yields `""`.
#[must_use]
pub fn credential_fingerprint(api_key: &str) -> String {
    if api_key.is_empty() {
        return String::new();
    }
    let digest = Sha256::digest(api_key.as_bytes());
    hex::encode(&digest[..8])
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn key_of(o: &Observation) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        o.provider,
        level_tag(o.level),
        o.credential,
        o.account,
        o.model,
        o.region
    )
}

fn level_tag(level: ScopeLevel) -> &'static str {
    match level {
        ScopeLevel::Credential => "cred",
        ScopeLevel::Account => "acct",
        ScopeLevel::Model => "model",
        ScopeLevel::Region => "region",
    }
}

/// Classify a provider failure into (kind, level) from status + body evidence.
/// Protocol/body wording is vendor knowledge, so it lives in this vendor crate.
#[must_use]
pub fn classify_failure(status: Option<u16>, body: &str) -> (EligibilityKind, ScopeLevel) {
    let b = body.to_ascii_lowercase();
    let says = |needles: &[&str]| needles.iter().any(|n| b.contains(n));
    if says(&[
        "insufficient_quota",
        "exceeded your current quota",
        "quota exhausted",
        "usage limit",
    ]) {
        return (EligibilityKind::QuotaExhausted, ScopeLevel::Account);
    }
    if says(&[
        "insufficient credit",
        "credit balance",
        "billing",
        "payment required",
        "out of credits",
    ]) {
        return (EligibilityKind::InsufficientCredit, ScopeLevel::Account);
    }
    if says(&["does not have access", "no access to", "model_not_found"]) && says(&["model"]) {
        return (EligibilityKind::AccessDenied, ScopeLevel::Model);
    }
    match status {
        Some(401) => (EligibilityKind::InvalidCredential, ScopeLevel::Credential),
        Some(402) => (EligibilityKind::InsufficientCredit, ScopeLevel::Account),
        Some(403) => (EligibilityKind::InvalidCredential, ScopeLevel::Credential),
        Some(429) => (EligibilityKind::RateLimited, ScopeLevel::Credential),
        Some(400) | Some(404) | Some(422) => {
            (EligibilityKind::UnsupportedRequest, ScopeLevel::Model)
        }
        Some(s) if s >= 500 => (EligibilityKind::ServiceUnavailable, ScopeLevel::Region),
        Some(_) => (EligibilityKind::Unknown, ScopeLevel::Credential),
        None => (EligibilityKind::ServiceUnavailable, ScopeLevel::Region),
    }
}

/// Best-effort region extraction for providers that encode it in the base URL
/// (AWS Bedrock/SageMaker, Azure OpenAI resource hosts).
#[must_use]
pub fn region_from_api_base(api_base: &str) -> Option<String> {
    let host = api_base
        .split("://")
        .nth(1)
        .unwrap_or(api_base)
        .split('/')
        .next()
        .unwrap_or("");
    for marker in ["amazonaws.com", "services.visualstudio.com"] {
        if host.ends_with(marker) {
            let mut labels = host.split('.').collect::<Vec<_>>();
            labels.pop();
            labels.pop();
            // e.g. bedrock-runtime.us-east-1.amazonaws.com → us-east-1
            return labels
                .last()
                .filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
                .map(|l| (*l).to_string());
        }
    }
    None
}

/// Bounded, redacting, expiring eligibility state. Paths are injected so
/// tests stay hermetic; production callers use [`record_inference_outcome`].
#[derive(Debug)]
pub struct EligibilityStore {
    observations: BTreeMap<String, Observation>,
    cap: usize,
    usable_ttl_secs: u64,
}

impl Default for EligibilityStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EligibilityStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            observations: BTreeMap::new(),
            cap: DEFAULT_CAP,
            usable_ttl_secs: USABLE_TTL_SECS,
        }
    }

    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        let mut s = Self::new();
        s.cap = cap.max(1);
        s
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.observations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    /// Insert an observation, redacting the reason against the credential
    /// material and any live `*_API_KEY`/`_TOKEN`/`_SECRET` env values.
    /// Over-capacity evicts expired entries first, then the oldest.
    pub fn record(&mut self, mut obs: Observation, secret: Option<&str>) {
        obs.reason = redact::mask_env_credentials(&obs.reason);
        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            obs.reason = obs.reason.replace(secret, "[REDACTED]");
        }
        if obs.reason.len() > REASON_CAP {
            obs.reason.truncate(REASON_CAP);
        }
        let key = key_of(&obs);
        self.observations.insert(key, obs);
        if self.observations.len() > self.cap {
            self.evict(now_unix());
        }
    }

    /// Record one real inference attempt's outcome. Success writes a `Usable`
    /// observation (fresh for `usable_ttl_secs`); failure writes whatever
    /// `classify_failure` scopes the provider evidence to.
    pub fn record_inference(&mut self, subject: Subject<'_>, result: &InferenceResult, now: u64) {
        let fp = credential_fingerprint(subject.api_key);
        let account = subject.account.unwrap_or_default();
        let region = subject.region.unwrap_or_default();
        let provider = subject.provider;
        let model = subject.model;
        match result {
            InferenceResult::Success => self.record(
                Observation {
                    kind: EligibilityKind::Usable,
                    level: ScopeLevel::Model,
                    provider: provider.to_string(),
                    credential: fp,
                    account: account.to_string(),
                    model: model.to_string(),
                    region: region.to_string(),
                    reason: "authenticated inference succeeded".to_string(),
                    observed_unix: now,
                    expires_unix: Some(now + self.usable_ttl_secs),
                    provenance: Provenance::Inference,
                },
                Some(subject.api_key),
            ),
            InferenceResult::Failed {
                status,
                body_snippet,
                retry_after_secs,
            } => {
                let (kind, level) = classify_failure(*status, body_snippet);
                let credential = match level {
                    ScopeLevel::Model => {
                        if kind == EligibilityKind::UnsupportedRequest {
                            "*".to_string()
                        } else {
                            fp.clone()
                        }
                    }
                    ScopeLevel::Region => "*".to_string(),
                    ScopeLevel::Credential | ScopeLevel::Account => fp.clone(),
                };
                let expires = match kind {
                    EligibilityKind::RateLimited => {
                        Some(now + retry_after_secs.unwrap_or(RATE_LIMIT_TTL_SECS))
                    }
                    EligibilityKind::ServiceUnavailable => Some(now + UNAVAILABLE_TTL_SECS),
                    EligibilityKind::QuotaExhausted | EligibilityKind::InsufficientCredit => {
                        Some(now + QUOTA_TTL_SECS)
                    }
                    EligibilityKind::Usable
                    | EligibilityKind::Unknown
                    | EligibilityKind::InvalidCredential
                    | EligibilityKind::AccessDenied
                    | EligibilityKind::UnsupportedRequest => None,
                };
                self.record(
                    Observation {
                        kind,
                        level,
                        provider: provider.to_string(),
                        credential,
                        account: account.to_string(),
                        model: model.to_string(),
                        region: region.to_string(),
                        reason: body_snippet.clone(),
                        observed_unix: now,
                        expires_unix: expires,
                        provenance: Provenance::Inference,
                    },
                    Some(subject.api_key),
                );
            }
        }
    }

    /// Resolve the effective eligibility for the subject at `now`.
    /// Non-expired observations only; blocks beat transients beat proven
    /// usability beat `Unknown`.
    #[must_use]
    pub fn resolve(&self, subject: Subject<'_>, now: u64) -> Verdict {
        let provider = subject.provider;
        let fp = credential_fingerprint(subject.api_key);
        let account = subject.account.unwrap_or_default();
        let region = subject.region.unwrap_or_default();
        let model = subject.model;
        let live = self
            .observations
            .values()
            .filter(|o| o.provider.eq_ignore_ascii_case(provider))
            .filter(|o| o.expires_unix.is_none_or(|e| e > now));

        let pick = |pred: &dyn Fn(&&Observation) -> bool| -> Option<Verdict> {
            live.clone()
                .filter(pred)
                .max_by_key(|o| o.observed_unix)
                .map(|o| Verdict {
                    kind: o.kind,
                    reason: o.reason.clone(),
                    observed_unix: o.observed_unix,
                })
        };

        // 1) the key itself is bad — blocks everything it touches.
        if let Some(v) = pick(&|o| {
            o.level == ScopeLevel::Credential
                && o.credential == fp
                && o.kind == EligibilityKind::InvalidCredential
        }) {
            return v;
        }
        // 2) account-level exhaustion applies to every key on the account;
        //    recorded per-credential when the account is unknown.
        if let Some(v) = pick(&|o| {
            matches!(
                o.kind,
                EligibilityKind::InsufficientCredit | EligibilityKind::QuotaExhausted
            ) && match o.level {
                ScopeLevel::Account => !account.is_empty() && o.account == account,
                ScopeLevel::Credential => o.credential == fp,
                ScopeLevel::Model | ScopeLevel::Region => false,
            }
        }) {
            return v;
        }
        // 3) model-scoped denials — never bleed onto sibling models.
        if let Some(v) = pick(&|o| {
            o.level == ScopeLevel::Model
                && o.model == model
                && matches!(
                    o.kind,
                    EligibilityKind::AccessDenied | EligibilityKind::UnsupportedRequest
                )
                && (o.credential == fp || o.credential == "*")
        }) {
            return v;
        }
        // 4) transient credential throttle.
        if let Some(v) = pick(&|o| {
            o.level == ScopeLevel::Credential
                && o.credential == fp
                && o.kind == EligibilityKind::RateLimited
        }) {
            return v;
        }
        // 5) provider/region outage: a provider-wide observation (empty
        //    region) hits everyone; a regional one only that region.
        if let Some(v) = pick(&|o| {
            o.kind == EligibilityKind::ServiceUnavailable
                && (o.region.is_empty() || o.region == region)
        }) {
            return v;
        }
        // 6) proven, fresh inference success for this pair.
        if let Some(v) = pick(&|o| {
            o.kind == EligibilityKind::Usable
                && o.level == ScopeLevel::Model
                && o.credential == fp
                && o.model == model
                && o.provenance == Provenance::Inference
        }) {
            return v;
        }
        Verdict {
            kind: EligibilityKind::Unknown,
            reason: "no observations".to_string(),
            observed_unix: 0,
        }
    }

    /// Drop expired observations; if still over capacity, evict oldest first.
    pub fn prune(&mut self, now: u64) {
        self.evict(now);
    }

    fn evict(&mut self, now: u64) {
        self.observations
            .retain(|_, o| o.expires_unix.is_none_or(|e| e > now));
        while self.observations.len() > self.cap {
            let oldest = self
                .observations
                .iter()
                .min_by_key(|(_, o)| o.observed_unix)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.observations.remove(&k);
                }
                None => break,
            }
        }
    }

    /// Persist to `path` as JSON (atomic write). Bounded and secret-free by
    /// construction — observations never carry raw credentials.
    pub fn save(&self, path: &Path) -> crate::susi_core::susi_error::EaiResult<()> {
        if let Some(parent) = path.parent() {
            crate::susi_config::create_private_dir(parent)
                .map_err(|e| crate::susi_core::susi_error::EaiError::io(e.to_string()))?;
        }
        crate::susi_config::atomic_write_json_pretty(
            path,
            &self.observations.values().collect::<Vec<_>>(),
        )
    }

    /// Load from `path`; a missing or unreadable file yields an empty store.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let mut s = Self::new();
        if let Ok(body) = std::fs::read_to_string(path) {
            if let Ok(rows) = serde_json::from_str::<Vec<Observation>>(&body) {
                for o in rows {
                    s.observations.insert(key_of(&o), o);
                }
            }
        }
        s
    }
}

fn default_path() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("cloud-eligibility.json")
}

static GLOBAL: OnceLock<Mutex<EligibilityStore>> = OnceLock::new();

fn global_store() -> &'static Mutex<EligibilityStore> {
    GLOBAL.get_or_init(|| Mutex::new(EligibilityStore::load(&default_path())))
}

/// Record one inference outcome into the process-global store and persist it.
/// `api_base` derives the provider region when the subject has none.
/// Best-effort: state tracking must never turn an inference call into a
/// failure, so a poisoned lock or unwritable path is swallowed.
pub fn record_inference_outcome(subject: Subject<'_>, api_base: &str, result: &InferenceResult) {
    let derived = region_from_api_base(api_base);
    let subject = Subject {
        region: subject.region.or(derived.as_deref()),
        ..subject
    };
    let mut store = global_store().lock().unwrap_or_else(|e| e.into_inner());
    store.record_inference(subject, result, now_unix());
    let _ = store.save(&default_path());
}

/// Record a management probe outcome (key check, `/models` listing).
/// Probe failures feed the same state; probe *success* is never proof of
/// usability — only inference is.
pub fn record_probe(provider: &str, api_key: &str, status: Option<u16>, detail: &str) {
    let (kind, level) = classify_failure(status, detail);
    if kind == EligibilityKind::Unknown {
        return;
    }
    let fp = credential_fingerprint(api_key);
    let mut store = global_store().lock().unwrap_or_else(|e| e.into_inner());
    store.record(
        Observation {
            kind,
            level,
            provider: provider.to_string(),
            credential: if level == ScopeLevel::Region {
                "*".to_string()
            } else {
                fp
            },
            account: String::new(),
            model: String::new(),
            region: String::new(),
            reason: detail.to_string(),
            observed_unix: now_unix(),
            expires_unix: match kind {
                EligibilityKind::RateLimited => Some(now_unix() + RATE_LIMIT_TTL_SECS),
                EligibilityKind::ServiceUnavailable => Some(now_unix() + UNAVAILABLE_TTL_SECS),
                EligibilityKind::QuotaExhausted | EligibilityKind::InsufficientCredit => {
                    Some(now_unix() + QUOTA_TTL_SECS)
                }
                EligibilityKind::Usable
                | EligibilityKind::Unknown
                | EligibilityKind::InvalidCredential
                | EligibilityKind::AccessDenied
                | EligibilityKind::UnsupportedRequest => None,
            },
            provenance: Provenance::Probe,
        },
        Some(api_key),
    );
    let _ = store.save(&default_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;

    fn subj<'a>(key: &'a str, model: &'a str) -> Subject<'a> {
        Subject {
            provider: "acme",
            api_key: key,
            account: None,
            region: None,
            model,
        }
    }

    fn subj_acct<'a>(key: &'a str, acct: &'a str, model: &'a str) -> Subject<'a> {
        Subject {
            provider: "acme",
            api_key: key,
            account: Some(acct),
            region: None,
            model,
        }
    }

    fn fail(status: u16, body: &str) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.to_string(),
            retry_after_secs: None,
        }
    }

    fn store_with_success() -> EligibilityStore {
        let mut s = EligibilityStore::new();
        s.record_inference(subj("sk-one", "m1"), &InferenceResult::Success, T0);
        s
    }

    #[test]
    fn cloud_eligibility_state_usable_requires_inference_success() {
        let mut s = EligibilityStore::new();
        // A configured key and a probe/listing success are not proof.
        s.record(
            Observation {
                kind: EligibilityKind::Usable,
                level: ScopeLevel::Model,
                provider: "acme".into(),
                credential: credential_fingerprint("sk-one"),
                account: String::new(),
                model: "m1".into(),
                region: String::new(),
                reason: "models listed".into(),
                observed_unix: T0,
                expires_unix: None,
                provenance: Provenance::Probe,
            },
            None,
        );
        assert_eq!(
            s.resolve(subj("sk-one", "m1"), T0 + 1).kind,
            EligibilityKind::Unknown,
            "listing provenance must not prove usability"
        );
        s.record_inference(subj("sk-one", "m1"), &InferenceResult::Success, T0 + 2);
        assert_eq!(
            s.resolve(subj("sk-one", "m1"), T0 + 3).kind,
            EligibilityKind::Usable
        );
    }

    #[test]
    fn cloud_eligibility_state_model_failure_isolates_siblings() {
        let mut s = store_with_success();
        s.record_inference(subj("sk-one", "m2"), &fail(404, "model not found"), T0);
        // m2 is unsupported for everyone; m1 still works for this key.
        assert_eq!(
            s.resolve(subj("sk-one", "m2"), T0 + 1).kind,
            EligibilityKind::UnsupportedRequest
        );
        assert_eq!(
            s.resolve(subj("sk-two", "m2"), T0 + 1).kind,
            EligibilityKind::UnsupportedRequest
        );
        assert_eq!(
            s.resolve(subj("sk-one", "m1"), T0 + 1).kind,
            EligibilityKind::Usable
        );
    }

    #[test]
    fn cloud_eligibility_state_account_limits_bind_shared_keys() {
        let mut s = EligibilityStore::new();
        // Two keys on one billing account; a third elsewhere.
        s.record_inference(
            subj_acct("sk-a", "acct-1", "m1"),
            &fail(402, "credit balance too low"),
            T0,
        );
        assert_eq!(
            s.resolve(subj_acct("sk-b", "acct-1", "m1"), T0 + 1).kind,
            EligibilityKind::InsufficientCredit,
            "a second key on the same account must not bypass the limit"
        );
        assert_eq!(
            s.resolve(subj_acct("sk-c", "acct-2", "m1"), T0 + 1).kind,
            EligibilityKind::Unknown,
            "other accounts are unaffected"
        );
    }

    #[test]
    fn cloud_eligibility_state_scopes_by_level() {
        let mut s = EligibilityStore::new();
        s.record_inference(subj("sk-bad", "m1"), &fail(401, "invalid api key"), T0);
        s.record_inference(subj("sk-ok", "m1"), &InferenceResult::Success, T0);
        assert_eq!(
            s.resolve(subj("sk-bad", "m9"), T0 + 1).kind,
            EligibilityKind::InvalidCredential,
            "a rejected key is dead for every model"
        );
        assert_eq!(
            s.resolve(subj("sk-ok", "m1"), T0 + 1).kind,
            EligibilityKind::Usable
        );
        assert_eq!(
            s.resolve(subj("sk-other", "m1"), T0 + 1).kind,
            EligibilityKind::Unknown
        );
    }

    #[test]
    fn cloud_eligibility_state_expiry_returns_to_unknown() {
        let mut s = EligibilityStore::new();
        s.record_inference(
            subj("sk-one", "m1"),
            &InferenceResult::Failed {
                status: Some(429),
                body_snippet: "slow down".into(),
                retry_after_secs: Some(60),
            },
            T0,
        );
        assert_eq!(
            s.resolve(subj("sk-one", "m1"), T0 + 30).kind,
            EligibilityKind::RateLimited
        );
        assert_eq!(
            s.resolve(subj("sk-one", "m1"), T0 + 120).kind,
            EligibilityKind::Unknown,
            "expired throttle must lift on its own"
        );
        // Usable freshness also expires.
        s.record_inference(subj("sk-one", "m1"), &InferenceResult::Success, T0);
        assert_eq!(
            s.resolve(subj("sk-one", "m1"), T0 + USABLE_TTL_SECS + 1)
                .kind,
            EligibilityKind::Unknown
        );
    }

    #[test]
    fn cloud_eligibility_state_persists_across_restart() {
        let dir = std::env::temp_dir().join(format!("susi-elig-{}", std::process::id()));
        let path = dir.join("elig.json");
        let mut s = store_with_success();
        s.record_inference(subj("sk-bad", "m1"), &fail(401, "bad key"), T0);
        s.save(&path).unwrap();
        let loaded = EligibilityStore::load(&path);
        assert_eq!(
            loaded.resolve(subj("sk-bad", "m1"), T0 + 1).kind,
            EligibilityKind::InvalidCredential
        );
        assert_eq!(
            loaded.resolve(subj("sk-one", "m1"), T0 + 1).kind,
            EligibilityKind::Usable
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_eligibility_state_redacts_secrets() {
        let mut s = EligibilityStore::new();
        let key = "sk-live-abcdefgh12345678";
        s.record_inference(
            subj(key, "m1"),
            &fail(401, &format!("key {key} was rejected",)),
            T0,
        );
        let dir = std::env::temp_dir().join(format!("susi-elig-redact-{}", std::process::id()));
        let path = dir.join("elig.json");
        s.save(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains(key), "persisted state leaked a credential");
        assert!(body.contains(&credential_fingerprint(key)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_eligibility_state_is_bounded() {
        let mut s = EligibilityStore::with_capacity(4);
        for i in 0..10u64 {
            s.record_inference(
                subj(&format!("sk-{i}"), &format!("m{i}")),
                &fail(503, "down"),
                T0 + i,
            );
        }
        assert!(s.len() <= 4, "store exceeded its cap: {}", s.len());
    }

    #[test]
    fn cloud_eligibility_state_classifies_provider_evidence() {
        assert_eq!(
            classify_failure(Some(429), "You exceeded your current quota"),
            (EligibilityKind::QuotaExhausted, ScopeLevel::Account)
        );
        assert_eq!(
            classify_failure(Some(429), "rate limit reached"),
            (EligibilityKind::RateLimited, ScopeLevel::Credential)
        );
        assert_eq!(
            classify_failure(Some(403), "insufficient credit balance"),
            (EligibilityKind::InsufficientCredit, ScopeLevel::Account)
        );
        assert_eq!(
            classify_failure(Some(403), "the model gpt-9 does not have access"),
            (EligibilityKind::AccessDenied, ScopeLevel::Model)
        );
        assert_eq!(
            classify_failure(Some(500), ""),
            (EligibilityKind::ServiceUnavailable, ScopeLevel::Region)
        );
        assert_eq!(
            classify_failure(None, "connection refused"),
            (EligibilityKind::ServiceUnavailable, ScopeLevel::Region)
        );
    }
}
