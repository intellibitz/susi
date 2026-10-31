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
//! adapters (aider, autogen, crewai, openhands, e2b, gemini-cli, haystack,
//! temporal, …). Provider output is evidence of execution, never proof that
//! the requested change is correct. SUSI crates consume this surface through
//! `susi_agents::external` (re-export) or this crate directly.

pub use susi_config;
pub use susi_core;
pub use susi_error;
pub use susi_sandbox_client as susi_sandbox;

pub mod a2a_card_cache;
pub mod agent_scoreboard;
pub mod external;
pub mod zc_a2a_registry;
pub mod zc_agent_approvals;
pub mod zc_delegation_dir;
pub mod zc_workspace_trust;
