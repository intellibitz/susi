//! Placement capability-floor, fitness, symmetric preference, and error
//! scoping invariants for [`crate::engines::routing::InferenceRouter`]
//! (`T-ANTIGRAVITYS1-4`, Vision V-2).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::engines::brain::{self, FailureKind, TaskClass};
use crate::engines::http_provider::{HttpProvider, InferenceProtocol};
use crate::engines::routing::{InferenceRouter, RoutingPreference};
use crate::susi_core::registry::CapabilityRegistry;

static SEQ: AtomicU64 = AtomicU64::new(0);

struct RoutingSandbox {
    tmp: PathBuf,
    _env: susi_paths::test_env::EnvGuard,
}

impl RoutingSandbox {
    fn new(tag: &str) -> Self {
        let tmp = std::env::temp_dir().join(format!(
            "susi_routing_placement_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let config = tmp.join("config");
        let _ = std::fs::create_dir_all(&config);
        let _ = std::fs::create_dir_all(tmp.join(".susi"));
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", &tmp);
        env.set("XDG_CONFIG_HOME", &config);
        env.set("SUSI_XDG", "0");
        env.set("SUSI_COOLDOWNS_FILE", tmp.join("cooldowns.json"));
        env.set("SUSI_BRAIN_EVIDENCE_FILE", tmp.join("brain.json"));
        let _ = brain::reset();
        Self { tmp, _env: env }
    }
}

impl Drop for RoutingSandbox {
    fn drop(&mut self) {
        let _ = brain::reset();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn cloud(name: &str) -> HttpProvider {
    HttpProvider {
        name: name.into(),
        api_base: "https://api.example.com/v1".into(),
        model: "m".into(),
        protocol: InferenceProtocol::OpenAiChat,
        api_key: String::new(),
    }
}

#[test]
fn placement_preserves_capability_floor_ahead_of_static_vendor_rank() {
    let _guard = crate::engines::env_test_lock();
    let _sb = RoutingSandbox::new("floor");
    InferenceRouter::save_preference(&RoutingPreference {
        policy_override: Some("cloud_first".into()),
        ..Default::default()
    });

    let registry = CapabilityRegistry::new();
    // openrouter (rank 0) and openai-gpt-4.1-mini (rank 1) fail the Reasoning floor;
    // groq-llama-70b (rank 8) meets Reasoning floor and must win over rank.
    registry.register_provider(cloud("openrouter-qwen-free"));
    registry.register_provider(cloud("openai-gpt-4.1-mini"));
    registry.register_provider(cloud("groq-llama-70b"));

    let decision =
        InferenceRouter::plan_placement_inner(Some("reasoning"), None, true, |_| true, &registry);
    assert_eq!(decision.provider.as_deref(), Some("groq-llama-70b"));
    assert_eq!(
        decision.cloud_candidates.first().map(String::as_str),
        Some("groq-llama-70b")
    );

    // Vendor-level preference ("openai") selects the floor-meeting openai-gpt-4o
    // rather than an arbitrary HashMap-ordered Basic sibling.
    registry.register_provider(cloud("openai-gpt-4o"));
    InferenceRouter::save_preference(&RoutingPreference {
        policy_override: Some("cloud_first".into()),
        preferred_cloud: Some("openai".into()),
        ..Default::default()
    });
    let pref_decision =
        InferenceRouter::plan_placement_inner(Some("reasoning"), None, true, |_| true, &registry);
    assert_eq!(pref_decision.provider.as_deref(), Some("openai-gpt-4o"));
}

#[test]
fn placement_prefers_fit_provider_over_unfit_provider() {
    let _guard = crate::engines::env_test_lock();
    let _sb = RoutingSandbox::new("fit");
    InferenceRouter::save_preference(&RoutingPreference {
        policy_override: Some("cloud_first".into()),
        ..Default::default()
    });

    let unfit = "openrouter-gpt-4o";
    let fit = "groq-llama-70b";
    let registry = CapabilityRegistry::new();
    registry.register_provider(cloud(unfit));
    registry.register_provider(cloud(fit));

    for _ in 0..3 {
        brain::record_outcome(unfit, TaskClass::Chat, false, 100);
    }
    brain::note_failure("vendor:openrouter", FailureKind::Funds);
    assert!(brain::is_unfit(unfit, TaskClass::Chat));
    assert!(!brain::is_unfit(fit, TaskClass::Chat));

    let decision = InferenceRouter::plan_placement_inner(None, None, true, |_| true, &registry);
    assert_eq!(decision.provider.as_deref(), Some(fit));
    assert_eq!(
        decision.cloud_candidates.first().map(String::as_str),
        Some(fit)
    );
}

#[test]
fn failover_order_matches_preferred_cloud_symmetrically_and_ignores_empty() {
    let _guard = crate::engines::env_test_lock();
    let _sb = RoutingSandbox::new("sym");

    let registry = CapabilityRegistry::new();
    registry.register_provider(cloud("openrouter-free"));
    registry.register_provider(cloud("deepseek"));

    // Long preference token matching short registered provider id.
    InferenceRouter::save_preference(&RoutingPreference {
        preferred_cloud: Some("deepseek-deepseek-chat".into()),
        ..Default::default()
    });
    let order = InferenceRouter::cloud_failover_order(&registry);
    assert_eq!(order.first().map(String::as_str), Some("deepseek"));

    // Empty preference must not match every candidate and override rank order.
    InferenceRouter::save_preference(&RoutingPreference {
        preferred_cloud: Some(String::new()),
        ..Default::default()
    });
    let default_order = InferenceRouter::cloud_failover_order(&registry);
    assert_eq!(
        default_order.first().map(String::as_str),
        Some("openrouter-free")
    );
}

#[test]
fn record_failure_scopes_rate_limit_and_unreachable_via_classify_error() {
    let _guard = crate::engines::env_test_lock();
    let _sb = RoutingSandbox::new("scope");
    let pid = std::process::id();

    let rl_down = format!("rlscope{pid}-model-a");
    let rl_sibling = format!("rlscope{pid}-model-b");
    InferenceRouter::record_failure(&rl_down, "Rate limit exceeded on vendor key");
    assert!(InferenceRouter::provider_cooled(&rl_down));
    assert!(InferenceRouter::provider_cooled(&rl_sibling));
    InferenceRouter::clear_provider_cooldown(&rl_down);

    let net_down = format!("netscope{pid}-model-a");
    let net_sibling = format!("netscope{pid}-model-b");
    InferenceRouter::record_failure(&net_down, "upstream host unreachable");
    assert!(InferenceRouter::provider_cooled(&net_down));
    assert!(InferenceRouter::provider_cooled(&net_sibling));
    InferenceRouter::clear_provider_cooldown(&net_down);

    let gone_down = format!("gonescope{pid}-model-a");
    let gone_sibling = format!("gonescope{pid}-model-b");
    InferenceRouter::record_failure(&gone_down, "Model not found");
    assert!(InferenceRouter::provider_cooled(&gone_down));
    assert!(!InferenceRouter::provider_cooled(&gone_sibling));
    InferenceRouter::clear_provider_cooldown(&gone_down);
}
