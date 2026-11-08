#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub mod adapter_ops;
pub mod ambient;
pub mod auto_discovery;
pub mod auto_tune;
pub mod blackboard;
pub mod boot_trace;
pub mod cell_watcher;
pub mod composition;
pub mod context_adapters;
pub mod crown;
pub mod discovery_pipeline;
pub mod ecosystem_view;
pub mod elastic_scheduler;
pub mod engine_hooks;
pub mod engine_watchdog;
pub mod gmcp_bootstrap;
pub mod gossip;
pub mod harness_ops;
pub mod host_views;
pub mod hot_reload_coverage;
pub mod nat;
pub mod orchestrator;
pub mod os_provision;
pub mod os_status;
pub mod plane_handler;
pub mod privacy;
pub mod provider_ops;
pub mod resource_governor;
pub mod runtime_admin;
pub mod runtime_view;
pub mod scheduled_missions;
pub mod security;
pub mod server;
pub mod service_control;
pub mod service_supervision;
pub mod sla_monitor;
pub mod status_report;
pub mod substrate_status;
pub mod supervisor;
pub mod swarm_metrics;
pub mod telemetry;
pub mod tls;
pub mod watchdog;
pub mod zc_daemon_lifecycle;
pub mod zc_keys_health_boot;
pub mod zc_port_autoselect;
pub mod zc_rediscovery_adaptive;
pub mod zc_scorecard;
pub mod zc_startup_autofix;

#[cfg(test)]
#[path = "tests/estate_loop_unattended.rs"]
mod estate_loop_unattended_tests;

#[cfg(test)]
#[path = "tests/estate_autonomy_tiers.rs"]
mod estate_autonomy_tiers_tests;

#[cfg(test)]
#[path = "tests/vc_201_065_mastery.rs"]
mod vc_201_065_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_202_010_mastery.rs"]
mod vc_202_010_mastery_tests;

pub use composition::{
    swarm_host_snapshot, wire_cli_substrate, wire_cli_substrate_priming, wire_engine_hooks,
};
pub use engine_hooks::SusiEngineHooks;
pub use server::SusiDaemon;
pub use service_supervision::{RestartEvent, ServiceStatus, Supervisor, WorkerState};
// SusiAdmin and EvolutionManager are used via fully qualified names in tools.rs
pub mod eco_drift_alerts;
pub mod eco_probe_scheduler;
pub mod estate_loop;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn crown_primed_surface_exposes_all_four_subsystems_via_daemon_root() {
        let primed = std::hint::black_box(&crown::PRIMED);
        assert!(primed.coding_models);
        assert!(primed.execution_agents);
        assert!(primed.frameworks);
    }

    #[test]
    fn estate_loop_action_kind_serde_roundtrips_snake_case() {
        let kind = estate_loop::ActionKind::WarmModel;
        let json = serde_json::to_string(&kind).expect("serialize");
        assert_eq!(json, "\"warm_model\"");
        let decoded: estate_loop::ActionKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, kind);
    }

    #[test]
    fn susi_engine_hooks_reports_non_empty_package_version() {
        use susi_tools::EngineHooks;
        let v = SusiEngineHooks.engine_version();
        assert!(!v.is_empty());
    }

    #[test]
    fn susi_error_reexport_constructs_process_error_from_daemon_root() {
        let err = susi_error::EaiError::process("daemon test");
        assert!(err.to_string().contains("daemon test"));
    }
}
