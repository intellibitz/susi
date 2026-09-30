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

pub mod external;
pub mod zc_agent_approvals;
pub mod zc_agent_autodetect;
pub mod zc_devin_org;
pub mod zc_workspace_trust;
