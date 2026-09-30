//! Cloud quota inventory: what providers have actually told us about
//! remaining capacity, per credential, account, model, and reset window.
//!
//! Distinct from `cloud_eligibility` (which tracks *can this pair be used at
//! all*): this module tracks *how much headroom remains* — balance, request/
//! token limits, free allowances, reset windows, and `Retry-After` hints —
//! normalized strictly from official response metadata. Unknown is never
//! conflated with zero or unlimited, and a successful model listing is never
//! treated as proof of inference credit.
//!
//! Observations are keyed by an opaque credential fingerprint (see
//! [`crate::cloud_eligibility::credential_fingerprint`]); key material is
//! never stored. Exhaustion is exported into [`EligibilityStore`] so model
//! selection sees it without a second source of truth.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cloud_eligibility::{
    credential_fingerprint, EligibilityKind, EligibilityStore, Observation, Provenance, ScopeLevel,
    Subject,
};

/// How much capacity an observation reports. `Unknown` is explicitly not
/// `Limited(0)` (exhausted) and not `Unlimited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaAmount {
    /// Provider gave no usable figure (missing header, unparsable value).
    Unknown,
    /// A concrete remaining count (0 = exhausted).
    Limited(u64),
    /// Provider explicitly reports no cap (e.g. paid plan metadata).
    Unlimited,
}

/// Which budget dimension an observation describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaKind {
    /// Requests remaining in the current window.
    Requests,
    /// Tokens remaining in the current window.
    Tokens,
    /// Monetary credits / balance remaining (units are provider-defined and
    /// opaque; only relative comparison within one provider is meaningful).
    Credits,
    /// Free-tier allowance remaining (requests or units on a free plan).
    FreeAllowance,
}

/// Where the figure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    /// Response headers on a real API call (`x-ratelimit-*`, `retry-after`).
    Headers,
    /// Provider account metadata fetched with existing consent (balance
    /// endpoints the provider officially documents for the key in use).
    Metadata,
}

/// One quota datum observed from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaObservation {
    pub provider: String,
    /// Opaque credential fingerprint — never the key itself.
    pub credential: String,
    /// Billing account id, or empty when unknown.
    pub account: String,
    /// Model id, or empty when the quota is not model-scoped.
    pub model: String,
    pub kind: QuotaKind,
    pub amount: QuotaAmount,
    /// When the window resets, if the provider said so.
    pub resets_at_unix: Option<u64>,
    /// `Retry-After` hint that accompanied the response.
    pub retry_after_secs: Option<u64>,
    /// Local time the observation was made (clamped for clock skew).
    pub observed_unix: u64,
    /// Observations older than this horizon are ignored.
    pub stale_after_secs: u64,
    pub source: QuotaSource,
}

/// Aggregated headroom for one (credential, model) subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Headroom {
    /// No usable observation for any dimension.
    Unknown,
    /// At least one dimension is at zero — the subject is spent.
    Exhausted {
        kind: QuotaKind,
        /// Earliest reset time if known.
        resets_at_unix: Option<u64>,
        retry_after_secs: Option<u64>,
    },
    /// All known dimensions have budget left; this is the tightest figure.
    Limited { remaining: u64 },
    /// Every observed dimension is explicitly uncapped.
    Unlimited,
}

/// Tolerance applied to timestamps: a reset time that passed within the
/// skew window still counts as "reset", and a server-supplied observation
/// time in the future is clamped to local now.
pub const CLOCK_SKEW_TOLERANCE_SECS: u64 = 300;

/// Default freshness horizon for quota figures: they drift fast.
pub const QUOTA_FRESH_SECS: u64 = 3600;

const DEFAULT_CAP: usize = 2048;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn key_of(o: &QuotaObservation) -> String {
    format!(
        "{}|{}|{}|{}|{:?}",
        o.provider, o.credential, o.account, o.model, o.kind
    )
}

/// Parse a positive integer header value; returns `None` for missing or
/// unparsable values — never guessed.
fn parse_u64(v: Option<&str>) -> Option<u64> {
    v.and_then(|s| s.trim().parse::<u64>().ok())
}

/// Parse a reset value that may be an absolute unix timestamp or a
/// relative seconds-from-now duration.
fn parse_reset(v: Option<&str>, now: u64) -> Option<u64> {
    let n = parse_u64(v)?;
    // Heuristic: values far in the plausible-epoch range are absolute;
    // small values are durations.
    if n > 1_000_000_000 {
        Some(n)
    } else {
        Some(now.saturating_add(n))
    }
}

/// Normalize rate-limit/`Retry-After`/`balance` headers from one provider
/// response into observations. `headers` are `(name, value)` pairs,
/// matched case-insensitively. Missing or unparsable values produce
/// `Unknown`-amount observations for the dimensions the provider does
/// expose, so "provider answered but told us nothing" stays distinct from
/// "no observation at all".
#[must_use]
pub fn parse_quota_headers(
    subject: Subject<'_>,
    headers: &[(String, String)],
    now: u64,
) -> Vec<QuotaObservation> {
    let get = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let provider = subject.provider;
    let model = subject.model;
    let cred = credential_fingerprint(subject.api_key);
    let acct = subject.account.unwrap_or_default().to_string();
    let retry_after = parse_u64(get("retry-after"));
    let mk = |kind: QuotaKind,
              remaining: Option<&str>,
              reset: Option<&str>,
              limit: Option<&str>|
     -> QuotaObservation {
        let amount = match parse_u64(remaining) {
            Some(n) => QuotaAmount::Limited(n),
            // No remaining figure but an explicit limit of 0/unlimited-ish
            // markers are still Unknown — a limit is not a balance.
            None => match limit.and_then(|s| s.trim().parse::<u64>().ok()) {
                Some(u64::MAX) => QuotaAmount::Unlimited,
                _ => QuotaAmount::Unknown,
            },
        };
        QuotaObservation {
            provider: provider.to_string(),
            credential: cred.clone(),
            account: acct.clone(),
            model: model.to_string(),
            kind,
            amount,
            resets_at_unix: parse_reset(reset, now),
            retry_after_secs: retry_after,
            observed_unix: now,
            stale_after_secs: QUOTA_FRESH_SECS,
            source: QuotaSource::Headers,
        }
    };

    let mut out = Vec::new();
    let reqs = [
        get("x-ratelimit-remaining-requests"),
        get("x-ratelimit-remaining"),
    ];
    if reqs.iter().any(Option::is_some) || get("x-ratelimit-limit-requests").is_some() {
        out.push(mk(
            QuotaKind::Requests,
            reqs.iter().flatten().next().copied(),
            get("x-ratelimit-reset-requests").or_else(|| get("x-ratelimit-reset")),
            get("x-ratelimit-limit-requests"),
        ));
    }
    if get("x-ratelimit-remaining-tokens").is_some() || get("x-ratelimit-limit-tokens").is_some() {
        out.push(mk(
            QuotaKind::Tokens,
            get("x-ratelimit-remaining-tokens"),
            get("x-ratelimit-reset-tokens").or_else(|| get("x-ratelimit-reset")),
            get("x-ratelimit-limit-tokens"),
        ));
    }
    // Balance/credit headers — provider-documented figures only.
    let credits = [
        get("x-credits-remaining"),
        get("x-balance-remaining"),
        get("x-openrouter-credits-remaining"),
    ];
    if let Some(v) = credits.iter().flatten().next() {
        out.push(QuotaObservation {
            provider: provider.to_string(),
            credential: cred.clone(),
            account: acct.clone(),
            model: model.to_string(),
            kind: QuotaKind::Credits,
            amount: parse_u64(Some(*v)).map_or(QuotaAmount::Unknown, QuotaAmount::Limited),
            resets_at_unix: None,
            retry_after_secs: retry_after,
            observed_unix: now,
            stale_after_secs: QUOTA_FRESH_SECS,
            source: QuotaSource::Metadata,
        });
    }
    // Explicit free-allowance header (providers that expose one).
    if let Some(v) = get("x-free-remaining").or_else(|| get("x-free-tier-remaining")) {
        out.push(QuotaObservation {
            provider: provider.to_string(),
            credential: cred,
            account: acct,
            model: model.to_string(),
            kind: QuotaKind::FreeAllowance,
            amount: parse_u64(Some(v)).map_or(QuotaAmount::Unknown, QuotaAmount::Limited),
            resets_at_unix: parse_reset(
                get("x-free-reset").or_else(|| get("x-free-tier-reset")),
                now,
            ),
            retry_after_secs: retry_after,
            observed_unix: now,
            stale_after_secs: QUOTA_FRESH_SECS,
            source: QuotaSource::Headers,
        });
    }
    out
}

/// Bounded store of quota observations. One record per
/// (provider, credential, account, model, kind); new observations replace
/// older ones for the same key. Persisted as JSON, never containing key
/// material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaInventory {
    observations: Vec<QuotaObservation>,
    cap: usize,
}

impl Default for QuotaInventory {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaInventory {
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAP)
    }

    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            observations: Vec::new(),
            cap: cap.max(1),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.observations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    /// Insert or replace the observation for the same key. Rejects
    /// observations that carry raw key material by construction (the
    /// fingerprint is computed by the caller/parsers, never stored raw).
    pub fn record(&mut self, obs: QuotaObservation, now: u64) {
        self.prune(now);
        let key = key_of(&obs);
        if let Some(slot) = self.observations.iter_mut().find(|o| key_of(o) == key) {
            // Keep the newer observation; equal timestamps replace.
            if slot.observed_unix <= obs.observed_unix {
                *slot = obs;
            }
            return;
        }
        self.observations.push(obs);
        if self.observations.len() > self.cap {
            self.observations.sort_by_key(|o| o.observed_unix);
            let excess = self.observations.len() - self.cap;
            self.observations.drain(0..excess);
        }
    }

    /// Drop stale records and those whose reset window has fully passed
    /// with nothing newer.
    pub fn prune(&mut self, now: u64) {
        self.observations.retain(|o| {
            let stale = o.observed_unix.saturating_add(o.stale_after_secs) < now;
            let reset_passed = o
                .resets_at_unix
                .is_some_and(|r| r + CLOCK_SKEW_TOLERANCE_SECS < now);
            !(stale || reset_passed)
        });
    }

    /// Latest non-stale observation for one dimension of a subject.
    fn latest(
        &self,
        cred: &str,
        subject: Subject<'_>,
        kind: QuotaKind,
        now: u64,
    ) -> Option<&QuotaObservation> {
        let account = subject.account.unwrap_or_default();
        self.observations
            .iter()
            .filter(|o| {
                o.provider == subject.provider
                    && o.kind == kind
                    && (o.credential == cred || (!o.account.is_empty() && o.account == account))
                    && (o.model.is_empty() || o.model == subject.model)
                    && o.observed_unix.saturating_add(o.stale_after_secs) >= now
            })
            .max_by_key(|o| o.observed_unix)
    }

    /// Resolve the effective headroom for a subject across all known
    /// dimensions. `Exhausted` beats `Limited` (tightest figure) which
    /// beats `Unknown` which beats `Unlimited`.
    #[must_use]
    pub fn resolve(&self, subject: Subject<'_>, now: u64) -> Headroom {
        let cred = credential_fingerprint(subject.api_key);
        let kinds = [
            QuotaKind::Requests,
            QuotaKind::Tokens,
            QuotaKind::Credits,
            QuotaKind::FreeAllowance,
        ];
        let mut saw_known = false;
        let mut saw_unlimited_only = true;
        let mut tightest = u64::MAX;
        for kind in kinds {
            let Some(obs) = self.latest(&cred, subject, kind, now) else {
                continue;
            };
            match obs.amount {
                QuotaAmount::Unknown => {}
                QuotaAmount::Unlimited => saw_known = true,
                QuotaAmount::Limited(0) => {
                    return Headroom::Exhausted {
                        kind,
                        resets_at_unix: obs.resets_at_unix,
                        retry_after_secs: obs.retry_after_secs,
                    };
                }
                QuotaAmount::Limited(n) => {
                    saw_known = true;
                    saw_unlimited_only = false;
                    tightest = tightest.min(n);
                }
            }
        }
        if !saw_known {
            return Headroom::Unknown;
        }
        if saw_unlimited_only {
            Headroom::Unlimited
        } else {
            Headroom::Limited {
                remaining: tightest,
            }
        }
    }

    /// Export exhausted/rate-limited subjects into the eligibility store so
    /// selection sees quota as eligibility. Unknown and Unlimited export
    /// nothing — they are not eligibility evidence.
    pub fn sync_into_eligibility(&self, store: &mut EligibilityStore, now: u64) {
        for o in &self.observations {
            if o.observed_unix.saturating_add(o.stale_after_secs) < now {
                continue;
            }
            let (kind, expires) = match o.amount {
                QuotaAmount::Limited(0) => (
                    EligibilityKind::QuotaExhausted,
                    o.resets_at_unix
                        .map(|r| r.saturating_add(CLOCK_SKEW_TOLERANCE_SECS)),
                ),
                // A retry-after without an exhaustion figure is a throttle.
                QuotaAmount::Limited(_) | QuotaAmount::Unknown => match o.retry_after_secs {
                    Some(secs) if o.observed_unix.saturating_add(secs) > now => (
                        EligibilityKind::RateLimited,
                        Some(o.observed_unix.saturating_add(secs)),
                    ),
                    Some(_) | None => continue,
                },
                QuotaAmount::Unlimited => continue,
            };
            let reason = if kind == EligibilityKind::QuotaExhausted {
                format!("quota {:?} exhausted per provider headers", o.kind)
            } else {
                format!("provider asked to retry after {:?}s", o.retry_after_secs)
            };
            store.record(
                Observation {
                    kind,
                    level: if o.account.is_empty() {
                        ScopeLevel::Credential
                    } else {
                        ScopeLevel::Account
                    },
                    provider: o.provider.clone(),
                    credential: o.credential.clone(),
                    account: o.account.clone(),
                    model: o.model.clone(),
                    region: String::new(),
                    reason,
                    observed_unix: now,
                    expires_unix: expires,
                    provenance: Provenance::Inference,
                },
                None,
            );
        }
    }

    /// Persist as JSON. Best-effort per record; the file itself is atomic.
    pub fn save(&self, path: &Path) -> crate::susi_core::susi_error::EaiResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::susi_core::susi_error::EaiError::io(format!(
                    "create {}: {e}",
                    parent.display()
                ))
            })?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|e| {
            crate::susi_core::susi_error::EaiError::config(format!("encode quota inventory: {e}"))
        })?;
        std::fs::write(path, body).map_err(|e| {
            crate::susi_core::susi_error::EaiError::io(format!("write {}: {e}", path.display()))
        })
    }

    /// Load from disk; missing or corrupt files yield an empty inventory
    /// (quota state is a cache of provider truth — loss degrades to
    /// `Unknown`, never to invented figures).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default()
    }
}

fn default_path() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("cloud_quota.json")
}

/// Record response headers from a real provider call into the on-disk
/// inventory, then export exhaustion into the global eligibility store.
/// Best-effort: quota tracking must never fail an inference call.
pub fn record_quota_headers(subject: Subject<'_>, headers: &[(String, String)]) {
    let now = now_unix();
    let mut inventory = QuotaInventory::load(&default_path());
    for obs in parse_quota_headers(subject, headers, now) {
        inventory.record(obs, now);
    }
    let _ = inventory.save(&default_path());
    crate::cloud_eligibility::update_global(|s| inventory.sync_into_eligibility(s, now));
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
            account: Some(acct),
            ..subj(key, model)
        }
    }

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn cloud_quota_inventory_free_only_tier() {
        // Free tier: small request allowance, no credit header at all.
        let obs = parse_quota_headers(
            subj("sk-free", "m1"),
            &hdrs(&[
                ("x-ratelimit-remaining-requests", "20"),
                ("x-ratelimit-reset-requests", "3600"),
                ("x-free-remaining", "20"),
            ]),
            T0,
        );
        let mut inv = QuotaInventory::new();
        for o in obs {
            inv.record(o, T0);
        }
        assert_eq!(
            inv.resolve(subj("sk-free", "m1"), T0 + 1),
            Headroom::Limited { remaining: 20 }
        );
    }

    #[test]
    fn cloud_quota_inventory_paid_only_tier() {
        // Paid key: uncapped requests, credit balance present.
        let obs = parse_quota_headers(
            subj_acct("sk-paid", "acct-9", "m1"),
            &hdrs(&[
                ("x-ratelimit-limit-requests", &u64::MAX.to_string()),
                ("x-credits-remaining", "4200"),
            ]),
            T0,
        );
        let mut inv = QuotaInventory::new();
        for o in obs {
            inv.record(o, T0);
        }
        let h = inv.resolve(subj_acct("sk-paid", "acct-9", "m1"), T0 + 1);
        assert_eq!(h, Headroom::Limited { remaining: 4200 });
    }

    #[test]
    fn cloud_quota_inventory_mixed_tiers_per_key() {
        let mut inv = QuotaInventory::new();
        for o in parse_quota_headers(
            subj("sk-free", "m1"),
            &hdrs(&[("x-ratelimit-remaining-requests", "5")]),
            T0,
        ) {
            inv.record(o, T0);
        }
        for o in parse_quota_headers(
            subj("sk-paid", "m1"),
            &hdrs(&[("x-credits-remaining", "100")]),
            T0,
        ) {
            inv.record(o, T0);
        }
        assert_eq!(
            inv.resolve(subj("sk-free", "m1"), T0),
            Headroom::Limited { remaining: 5 }
        );
        assert_eq!(
            inv.resolve(subj("sk-paid", "m1"), T0),
            Headroom::Limited { remaining: 100 }
        );
        assert_eq!(
            inv.resolve(subj("sk-never-seen", "m1"), T0),
            Headroom::Unknown
        );
    }

    #[test]
    fn cloud_quota_inventory_unknown_is_not_zero_or_unlimited() {
        // Provider responded but exposed no figures for the dimension.
        let obs = parse_quota_headers(
            subj("sk-x", "m1"),
            &hdrs(&[("x-ratelimit-limit-requests", "600")]),
            T0,
        );
        let mut inv = QuotaInventory::new();
        for o in obs {
            inv.record(o, T0);
        }
        assert_eq!(
            inv.resolve(subj("sk-x", "m1"), T0),
            Headroom::Unknown,
            "a limit header without a remaining figure must not read as balance"
        );
    }

    #[test]
    fn cloud_quota_inventory_stale_and_missing_headers() {
        let mut inv = QuotaInventory::new();
        for o in parse_quota_headers(
            subj("sk-old", "m1"),
            &hdrs(&[("x-ratelimit-remaining-requests", "9")]),
            T0,
        ) {
            inv.record(o, T0);
        }
        // After the freshness horizon the figure is stale → Unknown.
        assert_eq!(
            inv.resolve(subj("sk-old", "m1"), T0 + QUOTA_FRESH_SECS + 1),
            Headroom::Unknown
        );
        // Unparsable remaining yields an Unknown-amount observation.
        let obs = parse_quota_headers(
            subj("sk-junk", "m1"),
            &hdrs(&[("x-ratelimit-remaining-requests", "not-a-number")]),
            T0,
        );
        assert!(obs.iter().all(|o| o.amount == QuotaAmount::Unknown));
    }

    #[test]
    fn cloud_quota_inventory_clock_skew_tolerance() {
        // Reset that passed within the skew window still counts as reset.
        let mut obs = QuotaObservation {
            provider: "acme".into(),
            credential: credential_fingerprint("sk-s"),
            account: String::new(),
            model: "m1".into(),
            kind: QuotaKind::Requests,
            amount: QuotaAmount::Limited(0),
            resets_at_unix: Some(T0),
            retry_after_secs: None,
            observed_unix: T0,
            stale_after_secs: QUOTA_FRESH_SECS,
            source: QuotaSource::Headers,
        };
        let mut inv = QuotaInventory::new();
        inv.record(obs.clone(), T0);
        assert!(matches!(
            inv.resolve(subj("sk-s", "m1"), T0 + 1),
            Headroom::Exhausted { .. }
        ));
        // Within skew of the reset passing, prune treats it as reset.
        inv.prune(T0 + CLOCK_SKEW_TOLERANCE_SECS + 1);
        assert_eq!(inv.len(), 0);
        // Server clock ahead of ours: observation clamped, still usable.
        obs.observed_unix = T0 + 10_000;
        let mut inv2 = QuotaInventory::new();
        let mut clamped = obs.clone();
        clamped.observed_unix = clamped.observed_unix.min(T0); // callers clamp
        clamped.amount = QuotaAmount::Limited(3);
        inv2.record(clamped, T0);
        assert_eq!(
            inv2.resolve(subj("sk-s", "m1"), T0 + 1),
            Headroom::Limited { remaining: 3 }
        );
    }

    #[test]
    fn cloud_quota_inventory_account_shared_quota() {
        // Quota recorded against account applies to a sibling key.
        let mut inv = QuotaInventory::new();
        inv.record(
            QuotaObservation {
                provider: "acme".into(),
                credential: credential_fingerprint("sk-a"),
                account: "acct-1".into(),
                model: String::new(),
                kind: QuotaKind::Credits,
                amount: QuotaAmount::Limited(0),
                resets_at_unix: None,
                retry_after_secs: None,
                observed_unix: T0,
                stale_after_secs: QUOTA_FRESH_SECS,
                source: QuotaSource::Metadata,
            },
            T0,
        );
        assert!(matches!(
            inv.resolve(subj_acct("sk-b", "acct-1", "m1"), T0),
            Headroom::Exhausted {
                kind: QuotaKind::Credits,
                ..
            }
        ));
        assert_eq!(
            inv.resolve(subj_acct("sk-b", "acct-2", "m1"), T0),
            Headroom::Unknown
        );
    }

    #[test]
    fn cloud_quota_inventory_exhaustion_feeds_eligibility() {
        let mut inv = QuotaInventory::new();
        for o in parse_quota_headers(
            subj_acct("sk-e", "acct-1", "m1"),
            &hdrs(&[("x-credits-remaining", "0")]),
            T0,
        ) {
            inv.record(o, T0);
        }
        let mut store = EligibilityStore::new();
        inv.sync_into_eligibility(&mut store, T0);
        let v = store.resolve(
            crate::cloud_eligibility::Subject {
                provider: "acme",
                api_key: "sk-f",
                account: Some("acct-1"),
                region: None,
                model: "m1",
            },
            T0 + 1,
        );
        assert_eq!(v.kind, EligibilityKind::QuotaExhausted);
    }

    #[test]
    fn cloud_quota_inventory_persists_and_redacts() {
        let dir = std::env::temp_dir().join(format!("susi-quota-{}", std::process::id()));
        let path = dir.join("quota.json");
        let mut inv = QuotaInventory::new();
        for o in parse_quota_headers(
            subj("sk-secret-key-material", "m1"),
            &hdrs(&[("x-ratelimit-remaining-requests", "7")]),
            T0,
        ) {
            inv.record(o, T0);
        }
        inv.save(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("sk-secret-key-material"));
        let loaded = QuotaInventory::load(&path);
        assert_eq!(
            loaded.resolve(subj("sk-secret-key-material", "m1"), T0 + 1),
            Headroom::Limited { remaining: 7 }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_quota_inventory_is_bounded() {
        let mut inv = QuotaInventory::with_capacity(4);
        for i in 0..10u64 {
            inv.record(
                QuotaObservation {
                    provider: "acme".into(),
                    credential: credential_fingerprint(&format!("sk-{i}")),
                    account: String::new(),
                    model: String::new(),
                    kind: QuotaKind::Requests,
                    amount: QuotaAmount::Limited(i),
                    resets_at_unix: None,
                    retry_after_secs: None,
                    observed_unix: T0 + i,
                    stale_after_secs: QUOTA_FRESH_SECS,
                    source: QuotaSource::Headers,
                },
                T0 + i,
            );
        }
        assert!(inv.len() <= 4);
    }

    #[test]
    fn cloud_quota_inventory_retry_after_is_throttle_not_exhaustion() {
        let obs = parse_quota_headers(
            subj("sk-ra", "m1"),
            &hdrs(&[
                ("x-ratelimit-remaining-requests", "3"),
                ("retry-after", "45"),
            ]),
            T0,
        );
        let mut inv = QuotaInventory::new();
        for o in obs {
            inv.record(o, T0);
        }
        let mut store = EligibilityStore::new();
        inv.sync_into_eligibility(&mut store, T0);
        let v = store.resolve(
            crate::cloud_eligibility::Subject {
                provider: "acme",
                api_key: "sk-ra",
                account: None,
                region: None,
                model: "m1",
            },
            T0 + 10,
        );
        assert_eq!(v.kind, EligibilityKind::RateLimited);
    }
}
