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

//! Third-party agent integrations — external task executors and framework
//! adapters (aider, autogen, crewai, openhands, e2b, gemini-cli, n8n,
//! temporal, …). Provider output is evidence of execution, never proof that
//! the requested change is correct. SUSI crates consume this surface through
//! `susi_agents::external` (re-export) or this crate directly.

pub use susi_config;
pub use susi_core;
pub use susi_error;
pub use susi_sandbox_client as susi_sandbox;

pub mod a2a_card_cache;
pub mod a2a_streaming_client;
pub mod acp_discovery;
pub mod agent_scoreboard;
pub mod agent_trace_import;
pub mod cloud_agent_cost_guard;
pub mod cursor_json_results;
pub mod delegation_ingress;
#[cfg(test)]
mod eco_acp_ibm;
#[cfg(test)]
mod eco_acp_zed;
#[cfg(test)]
mod eco_agent_frameworks;
#[cfg(test)]
mod eco_anp_agui;
#[cfg(test)]
mod eco_computer_use_tools;
pub mod external;
pub mod zc_a2a_registry;
pub mod zc_agent_approvals;
pub mod zc_agent_autodetect;
pub mod zc_agent_flags;
pub mod zc_cloud_agent_autoenable;
pub mod zc_delegation_dir;
pub mod zc_devin_org;
pub mod zc_openhands_docker;
pub mod zc_workspace_trust;
