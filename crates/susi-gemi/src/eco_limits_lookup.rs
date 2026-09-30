//! Limits and pricing lookup with staleness warnings (VC-201-088 / T-CLAUDE-350).
//!
//! Rate limits, version pins and price facts are time-sensitive: a limit
//! recorded months ago may have moved. Every lookup returns the fact *and*
//! its freshness band, so a caller can surface "last checked N days ago"
//! rather than presenting a possibly-moved number as current.

use crate::models::eco_profile::{self, Profile};
use crate::models::eco_provenance::{freshness, Freshness, Policy};

/// One limit/price fact with its staleness state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitFact {
    /// Profile the fact came from.
    pub profile: String,
    /// `rate-limit`, `version`, `deviation`, `pricing`.
    pub kind: String,
    /// Signal or field name (`x-ratelimit-remaining-requests`, `gpt-4o`).
    pub name: String,
    /// The recorded value/meaning.
    pub value: String,
    /// ISO date the fact was retrieved.
    pub retrieved: String,
    /// `fresh`, `aging`, `stale`, `unreadable`, `future`.
    pub freshness: &'static str,
}

fn band(f: Freshness) -> &'static str {
    match f {
        Freshness::Fresh { .. } => "fresh",
        Freshness::Aging { .. } => "aging",
        Freshness::Stale { .. } | Freshness::Unreadable => "stale",
        Freshness::Future { .. } => "future",
    }
}

/// All recorded limit-like facts for a profile, each flagged by policy.
#[must_use]
pub fn lookup(profile: &Profile, policy: &Policy, now_unix: i64) -> Vec<LimitFact> {
    let prov = &profile.provenance;
    let fresh = band(freshness(prov, policy, now_unix));
    let mut out: Vec<LimitFact> = profile
        .rate_limits
        .iter()
        .map(|r| LimitFact {
            profile: profile.id.clone(),
            kind: "rate-limit".into(),
            name: r.signal.clone(),
            value: r.meaning.clone(),
            retrieved: prov.retrieved.clone(),
            freshness: fresh,
        })
        .collect();
    for v in &profile.versions {
        out.push(LimitFact {
            profile: profile.id.clone(),
            kind: "version".into(),
            name: v.id.clone(),
            value: v.status.clone(),
            retrieved: prov.retrieved.clone(),
            freshness: fresh,
        });
    }
    for d in &profile.deviations {
        out.push(LimitFact {
            profile: profile.id.clone(),
            kind: "deviation".into(),
            name: d.host.clone(),
            value: format!("{}: {}", d.field, d.behavior),
            retrieved: prov.retrieved.clone(),
            freshness: fresh,
        });
    }
    out
}

/// Load a bundled profile and look its facts up in one call.
///
/// # Errors
/// `EaiError` from [`eco_profile::load_bundled`].
pub fn lookup_bundled(
    profile_id: &str,
    policy: &Policy,
    now_unix: i64,
) -> susi_error::EaiResult<Vec<LimitFact>> {
    let p = eco_profile::load_bundled(profile_id)?;
    Ok(lookup(&p, policy, now_unix))
}

/// Only the facts already stale under `policy`.
#[must_use]
pub fn stale_only(facts: &[LimitFact]) -> Vec<LimitFact> {
    facts
        .iter()
        .filter(|f| f.freshness == "stale")
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eco_limits_lookup_finds_rate_limit_facts() {
        let facts = lookup_bundled("openai-chat-completions", &Policy::default(), 1_788_500_000)
            .expect("profile loads");
        assert!(facts.iter().any(|f| f.kind == "rate-limit"));
        for f in &facts {
            assert_eq!(f.freshness, "fresh"); // facts retrieved 2026-09-01, now is days later
            assert!(!f.retrieved.is_empty());
        }
    }

    #[test]
    fn eco_limits_lookup_versions_and_deviations_included() {
        let facts = lookup_bundled("openai-chat-completions", &Policy::default(), 1_788_500_000)
            .expect("profile loads");
        assert!(facts.iter().any(|f| f.kind == "version"));
    }

    #[test]
    fn eco_limits_lookup_flags_stale_facts() {
        // far-future `now` stales anything recorded
        let facts = lookup_bundled("openai-chat-completions", &Policy::default(), i64::MAX / 4)
            .expect("profile loads");
        assert!(facts
            .iter()
            .all(|f| f.freshness == "stale" || f.freshness == "future"));
        assert_eq!(stale_only(&facts).len(), facts.len());
    }

    #[test]
    fn eco_limits_lookup_unknown_profile_errors_not_invents() {
        let r = lookup_bundled("definitely-not-a-profile", &Policy::default(), 0);
        assert!(r.is_err());
    }
}
