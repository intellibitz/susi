//! Provider credential inventory and liveness scouting (VC-202-001).
//!
//! Every configured cloud credential is enumerated, probed with the
//! cheapest authenticated call the vendor accepts — a zero-token
//! `GET {api_base}/models` — and classified
//! live / expired / unauthorized / rate-limited / unknown with the
//! evidence recorded. Dead keys feed
//! [`susi_gemi_models::cloud_eligibility::record_probe`] into the
//! eligibility store (fingerprinted, never the secret) so failover steers
//! around them; keys themselves are never deleted. [`ScoutReport::format`]
//! renders the single report that answers "which credentials work today"
//! without spending meaningfully to find out.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::susi_core::inference_wire::InferenceProtocol;
use susi_gemi_models::cloud_eligibility::{credential_fingerprint, EligibilityKind};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Probe timeout: long enough to reach a slow vendor, short enough that a
/// dead endpoint cannot stall registration.
const SCOUT_TIMEOUT_SECS: u64 = 5;
/// Cap on the `/models` body kept for classification evidence.
const PROBE_BODY_CAP: u64 = 64 * 1024;
/// Registration runs on several startup paths; the scout re-probes at most
/// this often so repeat registrations stay free.
const SCOUT_MIN_INTERVAL_SECS: u64 = 300;

static LAST_SCOUT_UNIX: AtomicU64 = AtomicU64::new(0);

/// The liveness class VC-202-001 requires per credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// The cheapest call succeeded — the credential works today.
    Live,
    /// The credential authenticates but its account is spent (quota or
    /// credit exhaustion) — dead until refilled, not until rotated.
    Expired,
    /// The provider rejected the credential (401/403, access denied).
    Unauthorized,
    /// The provider throttled the probe (429).
    RateLimited,
    /// No classification is justified (transport failure, 5xx, unmapped
    /// response) — reported, never guessed.
    Unknown,
}

/// Map one probe response onto the liveness taxonomy, reusing the
/// provider-failure classifier so the scout and failover agree on what a
/// status/body means.
#[must_use]
pub fn classify_probe(status: Option<u16>, body: &str) -> Liveness {
    if let Some(s) = status {
        if (200..300).contains(&s) {
            return Liveness::Live;
        }
    }
    match susi_gemi_models::cloud_eligibility::classify_failure(status, body).0 {
        EligibilityKind::InvalidCredential | EligibilityKind::AccessDenied => {
            Liveness::Unauthorized
        }
        EligibilityKind::InsufficientCredit | EligibilityKind::QuotaExhausted => Liveness::Expired,
        EligibilityKind::RateLimited => Liveness::RateLimited,
        EligibilityKind::Usable
        | EligibilityKind::Unknown
        | EligibilityKind::ServiceUnavailable
        | EligibilityKind::UnsupportedRequest => Liveness::Unknown,
    }
}

/// One configured credential to scout. The secret rides along for the
/// probe but never enters a report or store — only its fingerprint does.
#[derive(Debug, Clone)]
pub struct CredentialTarget {
    /// Endpoint/vendor name (`openai`, `gemini`, …).
    pub vendor: String,
    /// Env var the key resolved from (display only; may be empty).
    pub env: String,
    /// Endpoint base URL.
    pub api_base: String,
    /// Wire protocol — picks the auth scheme and probe path.
    pub protocol: InferenceProtocol,
    /// The resolved secret — probed, fingerprinted, never recorded.
    pub api_key: String,
}

/// One probed credential's verdict — safe to log or persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialVerdict {
    pub vendor: String,
    pub env: String,
    pub liveness: Liveness,
    /// HTTP status observed, `None` on transport failure.
    pub status: Option<u16>,
    /// What the classification is based on — redacted, truncated.
    pub detail: String,
    /// Truncated SHA-256 fingerprint of the key — never the key.
    pub fingerprint: String,
    pub observed_unix: u64,
}

/// The single report: every configured credential, its liveness, and the
/// evidence behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoutReport {
    pub checked_unix: u64,
    pub verdicts: Vec<CredentialVerdict>,
}

impl ScoutReport {
    /// Verdicts for credentials that work today.
    pub fn live(&self) -> impl Iterator<Item = &CredentialVerdict> {
        self.verdicts
            .iter()
            .filter(|v| v.liveness == Liveness::Live)
    }

    /// One line per credential plus a summary — the report that answers
    /// "which credentials work today". Contains no secret material.
    #[must_use]
    pub fn format(&self) -> String {
        let mut out = String::new();
        let live = self.live().count();
        for v in &self.verdicts {
            let status = v
                .status
                .map_or_else(|| "no-response".to_string(), |s| s.to_string());
            out.push_str(&format!(
                "{}  {:>13}  {}  fp:{}  {}\n",
                v.vendor,
                format!("{:?}", v.liveness).to_lowercase(),
                status,
                v.fingerprint,
                v.detail
            ));
        }
        out.push_str(&format!(
            "{} credential(s) scouted: {} live, {} dead or unclassified\n",
            self.verdicts.len(),
            live,
            self.verdicts.len() - live
        ));
        out
    }
}

/// The cheapest authenticated call, injected so tests never touch the
/// network. Returns (status, body); `Err` is a transport failure.
pub trait CheapCall {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String>;
}

/// Production probe over `susi_http_transport`.
pub struct TransportCall;

impl CheapCall for TransportCall {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
        let call = susi_http_transport::http_call("GET", url, headers, SCOUT_TIMEOUT_SECS, 0)?;
        let status = call.status;
        let bytes = call.into_bytes(PROBE_BODY_CAP).map_err(|e| e.to_string())?;
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// The zero-token probe each protocol accepts: `GET {base}/models` with
/// the credential in that vendor's auth header. A keyless protocol gets
/// no credential-bearing probe.
#[must_use]
pub fn probe_request(
    protocol: InferenceProtocol,
    api_base: &str,
    api_key: &str,
) -> (String, Vec<(String, String)>) {
    let base = api_base.trim_end_matches('/');
    match protocol {
        InferenceProtocol::Anthropic => (
            format!("{base}/models"),
            vec![
                ("x-api-key".to_string(), api_key.to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ],
        ),
        InferenceProtocol::Gemini => (
            format!("{base}/models"),
            vec![("x-goog-api-key".to_string(), api_key.to_string())],
        ),
        InferenceProtocol::Triton => (api_base.to_string(), Vec::new()),
        InferenceProtocol::OpenAiChat | InferenceProtocol::OpenAiCompletions => {
            let mut headers = Vec::new();
            if !api_key.is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
            }
            if susi_gemi_models::openrouter::is_openrouter_base(api_base) {
                let (referer, title) = susi_gemi_models::openrouter::attribution_headers();
                headers.push(("HTTP-Referer".to_string(), referer));
                headers.push(("X-Title".to_string(), title));
            }
            (format!("{base}/models"), headers)
        }
    }
}

/// Probe each target and classify the response. Pure over its inputs —
/// enumeration, transport and persistence all live outside.
#[must_use]
pub fn scout_targets(call: &dyn CheapCall, targets: &[CredentialTarget]) -> ScoutReport {
    let checked = now_unix();
    let verdicts = targets
        .iter()
        .map(|t| {
            let (url, headers) = probe_request(t.protocol, &t.api_base, &t.api_key);
            let refs: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let (status, body) = match call.get(&url, &refs) {
                Ok((s, b)) => (Some(s), b),
                Err(e) => (None, e),
            };
            let liveness = classify_probe(status, &body);
            // Evidence must never carry the secret back out: strip the key
            // (and the token tail it may have glued to itself) before the
            // body snippet becomes the recorded detail.
            let snippet: String = body.chars().take(160).collect();
            let redacted =
                susi_error::redact::redact_patterns(std::slice::from_ref(&t.api_key), &snippet);
            let detail = match status {
                Some(s) => format!("HTTP {s}: {redacted}"),
                None => redacted,
            };
            CredentialVerdict {
                vendor: t.vendor.clone(),
                env: t.env.clone(),
                liveness,
                status,
                detail,
                fingerprint: credential_fingerprint(&t.api_key),
                observed_unix: checked,
            }
        })
        .collect();
    ScoutReport {
        checked_unix: checked,
        verdicts,
    }
}

/// Every configured remote-cloud credential: keyed endpoints only —
/// local engines have no credential to scout.
#[must_use]
pub fn configured_targets() -> Vec<CredentialTarget> {
    susi_gemi_models::cloud::effective_inference_endpoints()
        .into_iter()
        .filter(|e| susi_gemi_models::cloud::is_remote_cloud(&e.api_base))
        .filter_map(|e| {
            let key = susi_gemi_models::cloud::resolve_api_key(&e.api_key_env, &e.name);
            if key.is_empty() {
                return None;
            }
            Some(CredentialTarget {
                vendor: e.name,
                env: e.api_key_env,
                api_base: e.api_base,
                protocol: InferenceProtocol::from_config(&e.protocol_type),
                api_key: key,
            })
        })
        .collect()
}

/// Enumerate, probe, classify and record. Failure evidence feeds the
/// eligibility store so failover steers around dead keys; a probe success
/// is recorded in the report only — usability stays inference-earned.
#[must_use]
pub fn scout_configured(call: &dyn CheapCall) -> ScoutReport {
    let targets = configured_targets();
    let report = scout_targets(call, &targets);
    for (target, verdict) in targets.iter().zip(&report.verdicts) {
        susi_gemi_models::cloud_eligibility::record_probe(
            &target.vendor,
            &target.api_key,
            verdict.status,
            &verdict.detail,
        );
    }
    report
}

/// Production entry point wired into endpoint registration: scout at most
/// once per [`SCOUT_MIN_INTERVAL_SECS`] so repeat registrations are free.
/// Returns `None` when the interval has not elapsed.
pub fn scout_configured_once(call: &dyn CheapCall) -> Option<ScoutReport> {
    let now = now_unix();
    if now.saturating_sub(LAST_SCOUT_UNIX.load(Ordering::Relaxed)) < SCOUT_MIN_INTERVAL_SECS {
        return None;
    }
    LAST_SCOUT_UNIX.store(now, Ordering::Relaxed);
    Some(scout_configured(call))
}
