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
//! Transitional re-export shell for `susi-sandbox-client` after the Docker
//! (bollard) backend moved to `susi-commercial-apps` (`susi-fleet-command`).

pub use susi_config;
pub use susi_error;
pub use susi_sandbox_client::{
    audit_chain, daemon_state, extensions, manager, versioned_store, SandboxManager,
    VersionedJsonStore,
};

mod docker;
pub use docker::execute_in_docker;

#[cfg(test)]
mod tests {
    #[test]
    fn re_exported_sandbox_client_and_error_types_resolve_through_crate_root() {
        assert_eq!(
            std::any::TypeId::of::<crate::SandboxManager>(),
            std::any::TypeId::of::<susi_sandbox_client::SandboxManager>()
        );
        assert_eq!(
            std::any::TypeId::of::<crate::VersionedJsonStore<String>>(),
            std::any::TypeId::of::<susi_sandbox_client::VersionedJsonStore<String>>()
        );
        let err = crate::susi_error::EaiError::process("sandbox-test");
        assert!(err.to_string().contains("sandbox-test"));
        let default_cfg = crate::susi_config::SusiConfig::default();
        assert!(!default_cfg.sandbox_image().is_empty());
    }
}
