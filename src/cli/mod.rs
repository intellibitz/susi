// CLI command modules
pub mod admin_cli;
pub mod agent_cli;
pub mod aider_cli;
pub mod ambient_cli;
pub mod auto_cli;
pub mod blackboard_cli;
pub mod broker_cli;
pub mod browser_use_cli;
pub mod catalog_plane_cli;
pub mod commits_cli;
pub mod context_graph_cli;
pub mod control_plane_cli;
pub mod crown_cli;
pub mod deerflow_cli;
pub mod defs;
pub mod execution_agent_cli;
pub mod extensions_cli;
pub mod framework_cli;
pub mod frontier_cli;
pub mod gemini_cli;
pub mod intent_cli;
pub mod keys_cli;
pub mod mcp_cli;
pub mod mission_cli;
pub mod model_cli;
pub mod open_weight_cli;
pub mod openclaw_cli;
pub mod openhands_cli;
pub mod openrouter_cli;
pub mod openviking_cli;
pub mod os_cli;
pub mod os_runtime;
pub mod patch_cli;
pub mod peers_cli;
pub mod plan_cli;
pub mod plane_cli;
pub mod privacy_cli;
pub mod python_engine_cli;
pub mod services_cli;
pub mod shell_cli;
pub mod substrate_cli;
pub mod swe_agent_cli;
pub mod telemetry_cli;
pub mod tx_cli;

/// Profile and accelerator this binary was compiled with — the dev
/// self-install records it and refuses to downgrade a better install.
pub(crate) fn build_identity() -> susi_sandbox::auto_install::BuildIdentity {
    let accelerator = if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "metal") {
        Some("metal")
    } else if cfg!(feature = "mkl") {
        Some("mkl")
    } else {
        None
    };
    susi_sandbox::auto_install::BuildIdentity {
        release: !cfg!(debug_assertions),
        accelerator,
    }
}
