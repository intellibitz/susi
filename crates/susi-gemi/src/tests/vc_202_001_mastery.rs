//! Mastery verification for VC-202-001: the live side of credential
//! scouting.
//!
//! `probe_credentialed_opt_in` is the only function that claims to check a
//! credential against its provider. This test pins down that it performs
//! no call at all — it is an opt-in stub that reports every probe kind as
//! Unsupported. That is exactly the "missing" recorded in
//! .agents/roadmap-verdicts/VC-202-001.json, kept executable here.

use crate::provider_contract::{probe_credentialed_opt_in, ProbeStatus};

/// Without `SUSI_PROVIDER_PROBE=1` the probe does not even produce a claim,
/// and with the gate on it returns Unsupported claims carrying "configure
/// endpoint to exercise" — no network call, no status code interpreted, no
/// live/expired/unauthorized/rate-limited/unknown classification. The
/// cheapest-possible-call probe the vector requires does not exist.
/// (One test, not two: the env flag is process-global, so gate-off and
/// gate-on are exercised serially here.)
#[test]
fn vc_202_001_mastery_credentialed_probe_is_a_stub_not_a_probe() {
    let prev = std::env::var("SUSI_PROVIDER_PROBE").ok();

    std::env::remove_var("SUSI_PROVIDER_PROBE");
    assert!(probe_credentialed_opt_in("openai", "gpt-4o", "2024-08").is_none());

    std::env::set_var("SUSI_PROVIDER_PROBE", "1");
    let claims = probe_credentialed_opt_in("openai", "gpt-4o", "2024-08")
        .expect("opt-in probe should return claims");
    assert!(!claims.is_empty());
    for claim in &claims {
        assert_eq!(
            claim.status,
            ProbeStatus::Unsupported,
            "the only live-probe path reports every check as unsupported"
        );
        assert!(
            claim.detail.contains("configure endpoint"),
            "stub detail names the missing capability: {claim:?}"
        );
        // There is no per-credential verdict: provider/model/version are
        // echoed from the request, not observed from a call.
        assert_eq!(claim.provider, "openai");
    }

    match prev {
        Some(v) => std::env::set_var("SUSI_PROVIDER_PROBE", v),
        None => std::env::remove_var("SUSI_PROVIDER_PROBE"),
    }
}
