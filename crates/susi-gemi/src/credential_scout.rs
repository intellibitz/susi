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
/// Registration runs on several startup paths, and in every composing CLI
/// command; the scout re-probes a host's credential set at most this often
/// so repeat registrations stay free ([`scout_configured_once`]).
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
        record_verdict(target, verdict);
    }
    report
}

/// Feed one probed verdict to the eligibility store.
fn record_verdict(target: &CredentialTarget, verdict: &CredentialVerdict) {
    susi_gemi_models::cloud_eligibility::record_probe(
        &target.vendor,
        &target.api_key,
        verdict.status,
        &verdict.detail,
    );
}

/// Production entry point wired into endpoint registration: scout at most
/// once per [`SCOUT_MIN_INTERVAL_SECS`] so repeat registrations are free.
/// Returns `None` when the interval has not elapsed.
///
/// The interval is the host's, not only the process's: every composing CLI
/// command registers endpoints, and with only an in-process clock each one
/// probed every keyed vendor live — a network round trip per key, up to
/// [`SCOUT_TIMEOUT_SECS`] each — before it did its work. The host's last
/// scouts are kept in [`SCOUT_GATE_FILE`], each under one digest of the host
/// and the credential set it covered (vendor, endpoint, protocol and key
/// fingerprint of each — never a key): adding, rotating or removing a key,
/// or pointing a vendor at another endpoint, makes a set that is scouted at
/// once, in this process or any other.
///
/// A gate hit replays that scout: the verdicts it saw are kept with the
/// gate (liveness, status and the redacted detail the eligibility store
/// records anyway — never a key, nor its fingerprint), and this process
/// records them into its own eligibility store and reports them exactly as
/// if it had probed. A process that skips the network therefore holds the
/// same evidence as one that scouted, whatever became of the shared
/// eligibility file meanwhile (another process rewriting it from its own
/// snapshot, a save that failed): nothing it decides rests on that file.
///
/// A scout holds the host's interval only when every probe got an answer
/// about the credential itself. One that did not reach the vendor, was
/// throttled, or met a server error (see [`transient`]) records what it saw
/// and leaves the next process to scout again, as every process did before:
/// a failed scout never stands in for a working key's next check.
pub fn scout_configured_once(call: &dyn CheapCall) -> Option<ScoutReport> {
    let now = now_unix();
    if now.saturating_sub(LAST_SCOUT_UNIX.load(Ordering::Relaxed)) < SCOUT_MIN_INTERVAL_SECS {
        return None;
    }
    // Claimed before the probes, as before, so a concurrent registration in
    // this process does not scout alongside.
    LAST_SCOUT_UNIX.store(now, Ordering::Relaxed);
    let targets = configured_targets();
    let gate = susi_paths::SusiDirs::config_dir().join(SCOUT_GATE_FILE);
    let stamp = credential_set_stamp(&host_name(), &targets);
    let (report, replayed_from) =
        scout_unless_recent(call, &targets, (&gate, &stamp), now, &mut record_verdict);
    if let Some(at) = replayed_from {
        // The host scouted this set at `at`: this process's interval runs
        // out when the host's does. Nothing is logged here: the caller
        // reports replayed verdicts as it reports probed ones.
        LAST_SCOUT_UNIX.store(at, Ordering::Relaxed);
    }
    Some(report)
}

/// The host's record of its last scouts, in the config directory.
pub const SCOUT_GATE_FILE: &str = "credential_scout.last.json";

/// Credential sets the gate remembers at once: the daemon's and a few
/// shells' can differ (a key exported in one shell only).
const SCOUT_GATE_ENTRIES: usize = 8;

/// The host's last scouts, newest first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScoutGate {
    scouts: Vec<GateEntry>,
}

/// One scout that held the interval: when, over which host and credential
/// set ([`credential_set_stamp`]), and what each probe saw, in the set's
/// canonical order ([`canonical_order`]) — so no vendor, key or fingerprint
/// is written, only what the eligibility store records anyway.
#[derive(Debug, Serialize, Deserialize)]
struct GateEntry {
    stamp: String,
    checked_unix: u64,
    #[serde(default)]
    outcomes: Vec<GateOutcome>,
}

/// One probe's answer, as [`scout_targets`] classified it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GateOutcome {
    liveness: Liveness,
    status: Option<u16>,
    detail: String,
    observed_unix: u64,
}

fn within_interval(at: u64, now: u64) -> bool {
    now >= at && now - at < SCOUT_MIN_INTERVAL_SECS
}

/// This host's name, as the OS reports it.
fn host_name() -> String {
    sysinfo::System::host_name().unwrap_or_default()
}

/// What decides what a scout of `target` probes: vendor, endpoint, protocol
/// and key fingerprint.
fn target_line(target: &CredentialTarget) -> String {
    format!(
        "{}|{}|{:?}|{}",
        target.vendor,
        target.api_base,
        target.protocol,
        credential_fingerprint(&target.api_key)
    )
}

/// Indices of `targets` sorted by [`target_line`]: the order a gate entry
/// keeps its outcomes in. Two sets with one stamp line up index by index.
fn canonical_order(targets: &[CredentialTarget]) -> Vec<usize> {
    let lines: Vec<String> = targets.iter().map(target_line).collect();
    let mut order: Vec<usize> = (0..targets.len()).collect();
    order.sort_by(|a, b| lines[*a].cmp(&lines[*b]));
    order
}

/// One digest of the host and of everything that decides what a scout of
/// `targets` probes ([`target_line`] of each, sorted). It names no key and
/// cannot be turned back into one.
fn credential_set_stamp(host: &str, targets: &[CredentialTarget]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(host.as_bytes());
    for index in canonical_order(targets) {
        digest.update(b"\n");
        digest.update(target_line(&targets[index]).as_bytes());
    }
    hex::encode(digest.finalize())
}

/// The report the gated scout `entry` gave for `targets` (the set its stamp
/// names), or `None` when the entry does not hold one outcome per target.
fn replay(entry: &GateEntry, targets: &[CredentialTarget]) -> Option<ScoutReport> {
    if entry.outcomes.len() != targets.len() {
        return None;
    }
    let mut verdicts: Vec<Option<CredentialVerdict>> = vec![None; targets.len()];
    for (index, outcome) in canonical_order(targets).into_iter().zip(&entry.outcomes) {
        let target = &targets[index];
        verdicts[index] = Some(CredentialVerdict {
            vendor: target.vendor.clone(),
            env: target.env.clone(),
            liveness: outcome.liveness,
            status: outcome.status,
            detail: outcome.detail.clone(),
            fingerprint: credential_fingerprint(&target.api_key),
            observed_unix: outcome.observed_unix,
        });
    }
    Some(ScoutReport {
        checked_unix: entry.checked_unix,
        verdicts: verdicts.into_iter().collect::<Option<Vec<_>>>()?,
    })
}

/// Whether `verdict` is about the moment rather than the credential: no
/// answer from the vendor, a throttle, a timeout or a server error. Such a
/// scout does not hold the host's interval.
fn transient(verdict: &CredentialVerdict) -> bool {
    match verdict.status {
        None => true,
        Some(status) => status == 408 || status == 429 || status >= 500,
    }
}

/// Scout `targets` and `record` each verdict — unless the gate file shows
/// the credential set `stamp` names was scouted less than
/// [`SCOUT_MIN_INTERVAL_SECS`] before `now`: then that scout's verdicts are
/// replayed (recorded, and returned as the report) along with when it ran.
/// The gate is written once the scout is done, and only when no verdict was
/// [`transient`]: a scout cut short, or one that failed to reach a vendor,
/// leaves the next process to scout.
fn scout_unless_recent(
    call: &dyn CheapCall,
    targets: &[CredentialTarget],
    (gate, stamp): (&std::path::Path, &str),
    now: u64,
    record: &mut dyn FnMut(&CredentialTarget, &CredentialVerdict),
) -> (ScoutReport, Option<u64>) {
    let read = || -> ScoutGate {
        std::fs::read(gate)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    };
    let replayed = read()
        .scouts
        .iter()
        .filter(|entry| entry.stamp == stamp && within_interval(entry.checked_unix, now))
        .find_map(|entry| replay(entry, targets));
    if let Some(report) = replayed {
        for (target, verdict) in targets.iter().zip(&report.verdicts) {
            record(target, verdict);
        }
        let at = report.checked_unix;
        return (report, Some(at));
    }
    let report = scout_targets(call, targets);
    for (target, verdict) in targets.iter().zip(&report.verdicts) {
        record(target, verdict);
    }
    if report.verdicts.iter().any(transient) {
        return (report, None);
    }
    let outcomes = canonical_order(targets)
        .into_iter()
        .map(|index| {
            let verdict = &report.verdicts[index];
            GateOutcome {
                liveness: verdict.liveness,
                status: verdict.status,
                detail: verdict.detail.clone(),
                observed_unix: verdict.observed_unix,
            }
        })
        .collect();
    // Read again: another process may have scouted another set meanwhile.
    let mut file = read();
    file.scouts
        .retain(|entry| entry.stamp != stamp && within_interval(entry.checked_unix, now));
    file.scouts.insert(
        0,
        GateEntry {
            stamp: stamp.to_string(),
            checked_unix: now,
            outcomes,
        },
    );
    file.scouts.truncate(SCOUT_GATE_ENTRIES);
    if let Ok(bytes) = serde_json::to_vec(&file) {
        // Best effort: an unwritten gate only means the next process scouts.
        let _ = crate::susi_config::atomic_write_bytes(gate, &bytes);
    }
    (report, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Counts probes; answers by the key's name: `dead…` is rejected,
    /// `flaky…` meets a server error, `offline…` gets no answer, anything
    /// else is live.
    struct Counting(Cell<usize>);

    impl CheapCall for Counting {
        fn get(&self, _url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
            self.0.set(self.0.get() + 1);
            let key = |prefix: &str| headers.iter().any(|(_, v)| v.contains(prefix));
            if key("dead") {
                return Ok((401, "{\"error\":\"invalid api key\"}".to_string()));
            }
            if key("flaky") {
                return Ok((503, "{\"error\":\"overloaded\"}".to_string()));
            }
            if key("offline") {
                return Err("connection refused".to_string());
            }
            Ok((200, "{\"data\":[]}".to_string()))
        }
    }

    fn target(vendor: &str, key: &str) -> CredentialTarget {
        CredentialTarget {
            vendor: vendor.to_string(),
            env: String::new(),
            api_base: format!("https://api.{vendor}.example/v1"),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: key.to_string(),
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-scout-gate-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const T0: u64 = 1_800_000_000;
    const HOST: &str = "host-a";

    /// [`scout_unless_recent`] as a `Result`: `Err` with when the replayed
    /// scout ran on a gate hit.
    fn gated(
        call: &dyn CheapCall,
        targets: &[CredentialTarget],
        (gate, host): (&std::path::Path, &str),
        now: u64,
        record: &mut dyn FnMut(&CredentialTarget, &CredentialVerdict),
    ) -> Result<ScoutReport, u64> {
        let stamp = credential_set_stamp(host, targets);
        match scout_unless_recent(call, targets, (gate, &stamp), now, record) {
            (report, None) => Ok(report),
            (_, Some(at)) => Err(at),
        }
    }

    #[test]
    fn the_scout_interval_holds_across_processes_until_the_credentials_change() {
        let dir = scratch("interval");
        let gate = dir.join(SCOUT_GATE_FILE);
        let probes = Counting(Cell::new(0));
        let recorded = Cell::new(0);
        let mut record = |_: &CredentialTarget, _: &CredentialVerdict| {
            recorded.set(recorded.get() + 1);
        };
        let mut scout = |targets: &[CredentialTarget], now: u64| {
            gated(&probes, targets, (&gate, HOST), now, &mut record)
        };
        let keys = vec![target("alpha", "key-one"), target("beta", "dead-two")];

        let report = scout(&keys, T0).unwrap();
        assert_eq!(report.verdicts.len(), 2);
        assert_eq!(probes.0.get(), 2);
        assert_eq!(scout(&keys, T0 + 60).unwrap_err(), T0, "another process");
        assert_eq!(probes.0.get(), 2, "no vendor was probed again");

        let rotated = vec![target("alpha", "key-three"), target("beta", "dead-two")];
        assert!(scout(&rotated, T0 + 61).is_ok(), "a rotated key is scouted");
        assert_eq!(probes.0.get(), 4);
        let added = vec![
            target("alpha", "key-three"),
            target("beta", "dead-two"),
            target("gamma", "key-four"),
        ];
        assert!(scout(&added, T0 + 62).is_ok(), "an added key is scouted");
        assert_eq!(probes.0.get(), 7);
        let mut moved = added.clone();
        moved[2].api_base = "https://eu.gamma.example/v1".into();
        assert!(
            scout(&moved, T0 + 63).is_ok(),
            "a moved endpoint is scouted"
        );
        assert_eq!(probes.0.get(), 10);

        // The shell that had only the first two keys is still inside its own
        // interval: each environment's set keeps its entry.
        assert_eq!(scout(&rotated, T0 + 64).unwrap_err(), T0 + 61);
        assert_eq!(scout(&added, T0 + 64).unwrap_err(), T0 + 62);
        assert_eq!(probes.0.get(), 10);

        let later = T0 + 61 + SCOUT_MIN_INTERVAL_SECS;
        assert!(scout(&rotated, later).is_ok(), "past the interval");
        assert_eq!(probes.0.get(), 12);
        assert_eq!(
            recorded.get(),
            19,
            "every probed verdict was recorded, and every replayed one"
        );

        let stamp = std::fs::read_to_string(&gate).unwrap();
        for key in ["key-one", "dead-two", "key-three", "key-four"] {
            assert!(!stamp.contains(key), "no key is stored: {stamp}");
            assert!(
                !stamp.contains(&credential_fingerprint(key)),
                "no key's fingerprint is stored either: {stamp}"
            );
        }
        assert!(!stamp.contains("alpha"), "nor which vendors: {stamp}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The same credentials on another host sharing the profile are that
    /// host's to scout.
    #[test]
    fn another_host_scouts_for_itself() {
        let dir = scratch("host");
        let gate = dir.join(SCOUT_GATE_FILE);
        let probes = Counting(Cell::new(0));
        let keys = vec![target("alpha", "key-one")];
        for (host, at) in [(HOST, T0), ("host-b", T0 + 1)] {
            assert!(
                gated(&probes, &keys, (&gate, host), at, &mut |_, _| {}).is_ok(),
                "{host}"
            );
        }
        assert_eq!(probes.0.get(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A scout that did not reach a vendor, or met a server error, records
    /// what it saw but does not hold the interval: the next process scouts
    /// again, as every process did before the interval was the host's.
    #[test]
    fn a_failed_scout_does_not_hold_the_interval() {
        let dir = scratch("failed");
        let gate = dir.join(SCOUT_GATE_FILE);
        let probes = Counting(Cell::new(0));
        for failing in ["offline-one", "flaky-one"] {
            let keys = vec![target("alpha", "key-two"), target("beta", failing)];
            let recorded = Cell::new(0);
            let mut record = |_: &CredentialTarget, _: &CredentialVerdict| {
                recorded.set(recorded.get() + 1);
            };
            for at in [T0, T0 + 1] {
                assert!(
                    gated(&probes, &keys, (&gate, HOST), at, &mut record).is_ok(),
                    "{failing} at {at}"
                );
            }
            assert_eq!(recorded.get(), 4, "each scout recorded what it saw");
        }
        assert_eq!(probes.0.get(), 8);
        assert!(!gate.exists(), "no failed scout held the interval");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A gate that cannot be read is no gate: the scout runs.
    #[test]
    fn a_damaged_gate_scouts() {
        let dir = scratch("damaged");
        let gate = dir.join(SCOUT_GATE_FILE);
        std::fs::write(&gate, "{not json").unwrap();
        let probes = Counting(Cell::new(0));
        let keys = vec![target("alpha", "key-one")];
        assert!(gated(&probes, &keys, (&gate, HOST), T0, &mut |_, _| {}).is_ok());
        assert_eq!(probes.0.get(), 1);
        assert!(gated(&probes, &keys, (&gate, HOST), T0 + 1, &mut |_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A process that skips the scout on the host's gate still holds the
    /// scout's evidence: the gated verdicts (the 401 included) are replayed
    /// into its own store and its report, from the gate alone. The shared
    /// eligibility file is not consulted: here it was rewritten by another
    /// process (or its save failed) after the scout, and the 401 is still
    /// recorded, exactly as a process that probed would record it.
    #[test]
    fn a_gated_process_still_records_the_scouts_verdicts() {
        let dir = scratch("replay");
        let gate = dir.join(SCOUT_GATE_FILE);
        let eligibility = dir.join("cloud-eligibility.json");
        let probes = Counting(Cell::new(0));
        let keys = vec![
            target("gamma", "key-three"),
            target("alpha", "key-one"),
            target("beta", "dead-two"),
        ];
        let mut first: Vec<(String, Option<u16>, String)> = Vec::new();
        let scouted = gated(&probes, &keys, (&gate, HOST), T0, &mut |t, v| {
            first.push((t.api_key.clone(), v.status, v.detail.clone()));
        })
        .unwrap();
        // Whatever process A saved is gone: a daemon rewrote the file from
        // its own snapshot, or A's save never landed.
        std::fs::write(&eligibility, "{\"observations\":[]}").unwrap();

        let mut second: Vec<(String, Option<u16>, String)> = Vec::new();
        // The same set, enumerated in another order by the next process.
        let reordered = vec![keys[2].clone(), keys[0].clone(), keys[1].clone()];
        let at = gated(&probes, &reordered, (&gate, HOST), T0 + 30, &mut |t, v| {
            second.push((t.api_key.clone(), v.status, v.detail.clone()));
        })
        .unwrap_err();
        assert_eq!(at, T0);
        assert_eq!(probes.0.get(), 3, "the second process probed nothing");
        let dead = second
            .iter()
            .find(|(key, _, _)| key == "dead-two")
            .expect("the dead key's verdict reached the second process");
        assert_eq!(dead.1, Some(401));
        assert!(dead.2.contains("invalid api key"), "{}", dead.2);
        first.sort();
        second.sort();
        assert_eq!(first, second, "the same verdicts, key by key");

        // The replayed report is the scouted one, in this process's order.
        let stamp = credential_set_stamp(HOST, &reordered);
        let (replayed, _) = scout_unless_recent(
            &probes,
            &reordered,
            (&gate, &stamp),
            T0 + 31,
            &mut |_, _| {},
        );
        assert_eq!(replayed.verdicts.len(), reordered.len());
        for (target, verdict) in reordered.iter().zip(&replayed.verdicts) {
            assert_eq!(verdict.fingerprint, credential_fingerprint(&target.api_key));
            let original = scouted
                .verdicts
                .iter()
                .find(|v| v.vendor == target.vendor)
                .unwrap();
            assert_eq!(verdict, original);
        }
        assert_eq!(
            replayed
                .verdicts
                .iter()
                .find(|v| v.vendor == "beta")
                .map(|v| v.liveness),
            Some(Liveness::Unauthorized)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A gate entry that does not carry one outcome per credential (an older
    /// gate, a hand-edited one) cannot be replayed: the process scouts.
    #[test]
    fn a_gate_entry_without_verdicts_is_scouted_again() {
        let dir = scratch("bare");
        let gate = dir.join(SCOUT_GATE_FILE);
        let probes = Counting(Cell::new(0));
        let keys = vec![target("alpha", "key-one"), target("beta", "dead-two")];
        let stamp = credential_set_stamp(HOST, &keys);
        std::fs::write(
            &gate,
            format!("{{\"scouts\":[{{\"stamp\":\"{stamp}\",\"checked_unix\":{T0}}}]}}"),
        )
        .unwrap();
        let mut statuses = Vec::new();
        assert!(gated(&probes, &keys, (&gate, HOST), T0 + 1, &mut |_, v| {
            statuses.push(v.status);
        })
        .is_ok());
        assert_eq!(probes.0.get(), 2);
        assert!(statuses.contains(&Some(401)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
