//! Quota window accounting on the production admission path (VC-202-021,
//! T-DEEPSEEK-119).
//!
//! Every cloud `provider.generate` admits through
//! `key_arbitration::try_acquire`; declared quota windows (daily UTC,
//! weekly UTC, rolling) now sit inside that gate, so an exhausted window
//! refuses the call with its reset time. `quota_headroom` exposes the
//! remaining allowance and `cloud_failover_order_scoped` demotes
//! nearly-spent keys — scarcity is not free. Tests inject the clock via
//! `KeyArbiter::with_clock` so reset semantics run deterministically.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::key_arbitration::{ArbiterLimits, Denial, KeyArbiter, QuotaWindowSpec, WindowKind};

const DAY: u64 = 86_400;
/// Unix 345600 is Monday 1970-01-05 00:00 UTC.
const MONDAY: u64 = 4 * DAY;

fn limits(quota: Vec<QuotaWindowSpec>) -> ArbiterLimits {
    ArbiterLimits {
        per_key_concurrent: 100,
        per_key_requests: u32::MAX,
        window_secs: 60,
        global_concurrent: 100,
        quota,
    }
}

fn fake_clock(start: u64) -> (Arc<AtomicU64>, Arc<dyn Fn() -> u64 + Send + Sync>) {
    let t = Arc::new(AtomicU64::new(start));
    let t2 = t.clone();
    (t, Arc::new(move || t2.load(Ordering::Relaxed)))
}

fn arbiter_at(spec: QuotaWindowSpec, now: u64) -> (KeyArbiter, Arc<AtomicU64>) {
    let (t, clock) = fake_clock(now);
    (KeyArbiter::with_clock(limits(vec![spec]), clock), t)
}

#[test]
fn quota_window_accounting_daily_resets_at_utc_midnight() {
    let spec = QuotaWindowSpec {
        kind: WindowKind::DailyUtc,
        allowance: 2,
    };
    let (arbiter, t) = arbiter_at(spec, 1_700_000_000);
    let reset = (1_700_000_000 / DAY + 1) * DAY;

    drop(arbiter.try_acquire("key").unwrap());
    let view = arbiter.scope_status("key").quota[0];
    assert_eq!(view.remaining, 1);
    assert_eq!(view.reset_unix, reset);

    drop(arbiter.try_acquire("key").unwrap());
    assert!(matches!(
        arbiter.try_acquire("key"),
        Err(Denial::Quota { reset_unix }) if reset_unix == reset
    ));

    t.store(reset, Ordering::Relaxed);
    drop(arbiter.try_acquire("key").unwrap());
    assert_eq!(arbiter.scope_status("key").quota[0].remaining, 1);
}

#[test]
fn quota_window_accounting_weekly_resets_monday_utc() {
    let spec = QuotaWindowSpec {
        kind: WindowKind::WeeklyUtc,
        allowance: 1,
    };
    // Window opens mid-week (Tuesday); reset must be next Monday 00:00 UTC.
    let open = MONDAY + DAY + 60;
    let (arbiter, t) = arbiter_at(spec, open);
    let reset = MONDAY + 7 * DAY;

    drop(arbiter.try_acquire("key").unwrap());
    let view = arbiter.scope_status("key").quota[0];
    assert_eq!(view.reset_unix, reset);
    assert!(matches!(
        arbiter.try_acquire("key"),
        Err(Denial::Quota { reset_unix }) if reset_unix == reset
    ));

    t.store(reset, Ordering::Relaxed);
    drop(arbiter.try_acquire("key").unwrap());
}

#[test]
fn quota_window_accounting_rolling_window_resets_after_period() {
    let spec = QuotaWindowSpec {
        kind: WindowKind::Rolling { period_secs: 3600 },
        allowance: 3,
    };
    let (arbiter, t) = arbiter_at(spec, 5_000);
    for _ in 0..3 {
        drop(arbiter.try_acquire("key").unwrap());
    }
    assert!(matches!(
        arbiter.try_acquire("key"),
        Err(Denial::Quota { reset_unix: 8_600 })
    ));
    t.store(8_599, Ordering::Relaxed);
    assert!(arbiter.try_acquire("key").is_err());
    t.store(8_600, Ordering::Relaxed);
    drop(arbiter.try_acquire("key").unwrap());
    assert_eq!(arbiter.scope_status("key").quota[0].reset_unix, 12_200);
}

#[test]
fn quota_window_accounting_multiple_windows_tightest_governs() {
    let (t, clock) = fake_clock(1_700_000_000);
    let arbiter = KeyArbiter::with_clock(
        limits(vec![
            QuotaWindowSpec {
                kind: WindowKind::DailyUtc,
                allowance: 2,
            },
            QuotaWindowSpec {
                kind: WindowKind::Rolling { period_secs: 3600 },
                allowance: 1,
            },
        ]),
        clock,
    );
    drop(arbiter.try_acquire("key").unwrap());
    // The rolling window (allowance 1) refuses before the daily one.
    assert!(matches!(
        arbiter.try_acquire("key"),
        Err(Denial::Quota { .. })
    ));
    // Headroom reports the tightest window: daily has 1 remaining but
    // rolling has 0.
    assert_eq!(arbiter.quota_headroom("key"), Some((0, 1)));
    t.store(1_700_000_000 + 3600, Ordering::Relaxed);
    drop(arbiter.try_acquire("key").unwrap());
}

#[test]
fn quota_window_accounting_headroom_scarcity_demotes_failover_order() {
    // Serialize against other tests that touch global preference/config.
    let _guard = crate::engines::env_test_lock();
    let tmp = std::env::temp_dir().join(format!(
        "susi_quota_scarcity_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(tmp.join("config"));
    let _ = std::fs::create_dir_all(tmp.join(".susi"));
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &tmp);
    env.set("XDG_CONFIG_HOME", tmp.join("config"));
    env.set("SUSI_XDG", "0");

    use crate::susi_core::registry::CapabilityRegistry;
    let registry = CapabilityRegistry::new();
    for name in ["openai-gpt-4o-mini", "deepseek-deepseek-chat"] {
        registry.register_provider(crate::engines::http_provider::HttpProvider {
            name: name.into(),
            api_base: "https://example.invalid/v1".into(),
            model: name.into(),
            protocol: crate::engines::http_provider::InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        });
    }

    let headroom = |name: &str| -> Option<(u64, u64)> {
        match name {
            "openai-gpt-4o-mini" => Some((0, 100)), // spent
            "deepseek-deepseek-chat" => Some((95, 100)),
            _ => None,
        }
    };
    let order = crate::engines::routing::InferenceRouter::cloud_failover_order_scoped(
        &registry,
        crate::engines::brain::TaskClass::Chat,
        &headroom,
    );
    let spent = order.iter().position(|n| n == "openai-gpt-4o-mini");
    let abundant = order.iter().position(|n| n == "deepseek-deepseek-chat");
    assert!(
        abundant < spent,
        "nearly-free cap must not outrank abundant headroom: {order:?}"
    );

    // Scarcity tiers: no quota declared is neutral, spent is worst.
    use crate::engines::routing::InferenceRouter;
    assert_eq!(InferenceRouter::quota_scarcity_tier(None), 0);
    assert_eq!(InferenceRouter::quota_scarcity_tier(Some((50, 100))), 0);
    assert_eq!(InferenceRouter::quota_scarcity_tier(Some((10, 100))), 1);
    assert_eq!(InferenceRouter::quota_scarcity_tier(Some((0, 100))), 2);
}

#[test]
fn quota_window_accounting_utilization_reflects_scarcity() {
    let spec = QuotaWindowSpec {
        kind: WindowKind::DailyUtc,
        allowance: 4,
    };
    let (arbiter, _t) = arbiter_at(spec, 1_700_000_000);
    assert_eq!(
        arbiter.scope_status("key").quota[0].utilization_percent(),
        0
    );
    drop(arbiter.try_acquire("key").unwrap());
    let view = arbiter.scope_status("key").quota[0];
    assert_eq!(view.utilization_percent(), 25);
    assert_eq!(view.remaining, 3);
}

#[test]
fn quota_window_accounting_per_key_windows_are_independent() {
    let spec = QuotaWindowSpec {
        kind: WindowKind::DailyUtc,
        allowance: 1,
    };
    let (arbiter, _t) = arbiter_at(spec, 1_700_000_000);
    drop(arbiter.try_acquire("key-a").unwrap());
    // key-a exhausted; key-b retains its own full allowance.
    assert!(arbiter.try_acquire("key-a").is_err());
    drop(arbiter.try_acquire("key-b").unwrap());
    assert_eq!(arbiter.quota_headroom("key-b"), Some((0, 1)));
    assert_eq!(arbiter.quota_headroom("key-a"), Some((0, 1)));
}

#[test]
fn quota_window_accounting_production_path_wired() {
    // The quota check must live inside the admission gate every production
    // cloud call passes through, and the failover ordering must read the
    // remaining allowance — not test-only machinery.
    let arbiter_src = include_str!("../key_arbitration.rs");
    let acquire = arbiter_src
        .split("pub fn try_acquire")
        .nth(1)
        .unwrap()
        .split("fn release")
        .next()
        .unwrap();
    assert!(
        acquire.contains("quota_windows") && acquire.contains("Denial::Quota"),
        "try_acquire must refuse on exhausted quota windows"
    );
    let routing_src = include_str!("../engines/routing.rs");
    assert!(
        routing_src.contains("crate::key_arbitration::quota_headroom")
            && routing_src.contains("quota_scarcity_tier"),
        "cloud_failover_order_for must consult the shared arbiter's quota headroom"
    );
    let runtime_src = include_str!("../engines/runtime.rs");
    assert!(
        runtime_src.contains("crate::key_arbitration::try_acquire"),
        "the production provider cascade must admit through the arbiter"
    );
}

#[test]
fn quota_window_accounting_env_declares_windows() {
    // Serialize against other env-mutating tests.
    let _guard = crate::engines::env_test_lock();
    // Parse coverage for the operator-facing declarations.
    std::env::set_var("SUSI_KEY_QUOTA_DAILY", "1000");
    std::env::set_var("SUSI_KEY_QUOTA_ROLLING", "3600:60");
    let parsed = crate::key_arbitration::limits_from_env();
    assert!(parsed
        .quota
        .iter()
        .any(|s| s.kind == WindowKind::DailyUtc && s.allowance == 1000));
    assert!(parsed
        .quota
        .iter()
        .any(|s| s.kind == WindowKind::Rolling { period_secs: 3600 } && s.allowance == 60));
    std::env::remove_var("SUSI_KEY_QUOTA_DAILY");
    std::env::remove_var("SUSI_KEY_QUOTA_ROLLING");
}
