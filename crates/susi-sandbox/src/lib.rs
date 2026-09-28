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

#[cfg(test)]
mod tests {
    /// With no docker daemon reachable, execution must fail as a typed
    /// `EaiError`, not panic or block forever.
    #[test]
    fn docker_execute_errors_cleanly_without_a_daemon() {
        std::env::set_var("DOCKER_HOST", "unix:///nonexistent-susi-test.sock");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(crate::execute_in_docker("echo hi"));
        assert!(res.is_err());
    }

    /// The container contract is the security boundary: it must stay
    /// network-less, capability-free and read-only.
    #[test]
    fn container_config_enforces_the_hardening_contract() {
        let cfg = crate::docker::hardened_container_config("susi-sandbox:test".to_string(), "id");
        assert_eq!(cfg.image.as_deref(), Some("susi-sandbox:test"));
        assert_eq!(cfg.network_disabled, Some(true));
        assert_eq!(
            cfg.cmd.as_deref(),
            Some(&["sh".to_string(), "-c".to_string(), "id".to_string()][..])
        );
        let host = cfg.host_config.expect("host config");
        assert_eq!(host.network_mode.as_deref(), Some("none"));
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.cap_drop.as_deref(), Some(&["ALL".to_string()][..]));
        assert_eq!(host.memory, Some(256 * 1024 * 1024));
        assert_eq!(host.nano_cpus, Some(500_000_000));
        assert_eq!(host.pids_limit, Some(128));
        assert_eq!(host.auto_remove, Some(true));
        assert_eq!(
            host.security_opt.as_deref(),
            Some(&["no-new-privileges:true".to_string()][..])
        );
    }
}
