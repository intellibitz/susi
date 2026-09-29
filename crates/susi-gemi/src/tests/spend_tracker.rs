use crate::spend_tracker::{SpendLimits, SpendTracker};
use std::collections::BTreeMap;

#[test]
fn spend_tracker_enforces_daily_hard_cap() {
    let mut limits = BTreeMap::new();
    limits.insert("openai".into(), 10.0);
    let mut t = SpendTracker::with_limits(SpendLimits { daily_usd: limits });
    t.reserve("2026-09-29", "openai", 6.0).unwrap();
    assert!(t.under_cap("2026-09-29", "openai"));
    let err = t.reserve("2026-09-29", "openai", 5.0).unwrap_err();
    assert!(err.contains("hard cap"));
    // Failed reserve does not debit; fill to the hard cap next.
    t.reserve("2026-09-29", "openai", 4.0).unwrap();
    assert!(!t.under_cap("2026-09-29", "openai"));
    assert_eq!(
        t.route_away_vendors("2026-09-29"),
        vec!["openai".to_string()]
    );
}

#[test]
fn spend_tracker_resets_per_day_and_unknown_vendor_unlimited() {
    let mut limits = BTreeMap::new();
    limits.insert("anthropic".into(), 1.0);
    let mut t = SpendTracker::with_limits(SpendLimits { daily_usd: limits });
    t.record("2026-09-29", "anthropic", 1.0);
    assert!(!t.under_cap("2026-09-29", "anthropic"));
    assert!(t.under_cap("2026-09-30", "anthropic"));
    t.reserve("2026-09-29", "local", 999.0).unwrap();
}
