#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-sandbox
//!
//! Docker (bollard) integration for the `susi-sandbox` leaf service, whose
//! HTTP shell lives in `susi-leaf-services` (default bind `127.0.0.1:18083`,
//! `SUSI_SANDBOX_PORT`). bollard is linked only by this crate; feature crates depend on `susi-sandbox-client`,
//! which also owns the helpers re-exported here. Audit HMAC key ops are never
//! exposed over HTTP.

pub use susi_config;
pub use susi_error;
pub use susi_sandbox_client::{
    audit_chain, daemon_state, extensions, manager, versioned_store, SandboxManager,
    VersionedJsonStore,
};

mod docker;
pub use docker::execute_in_docker;
