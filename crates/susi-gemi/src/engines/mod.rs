//! Inference **engines** tier: backends, protocol providers, and routing.
//!
//! This tier *runs* tokens. It may call [`crate::models`] (`susi-gemi-models`)
//! for path resolution, readiness, and selection — it owns Candle/HTTP/MCP
//! execution graphs.
//!
//! Each engine module has exactly one path, `susi_gemi::engines::<module>`.

mod alpha_avail;
mod alpha_pure;

pub mod alpha;
pub mod brain;
pub mod candle_provider;
pub mod capability;
pub mod cost;
pub mod http_provider;
pub mod local_plug;
pub mod mcp_provider;
pub mod native_lifecycle;
pub mod reflex;
pub mod reflex_llm;
pub mod routing;
/// Core local/cloud inference runtime (`GemiEngine`, `InferenceHost`, …).
pub mod runtime;

/// Serializes every test in this crate that mutates the process-global
/// `HOME`/`XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`SUSI_XDG` env vars, or that
/// reads/writes a real file under `susi_paths::SusiDirs::config_dir()`
/// (which resolves through those same env vars). Env var mutation is
/// visible to every thread immediately, so two tests in different files
/// that each use their own private lock (as `routing.rs` and
/// `http_provider.rs` used to) can still race against each other, since
/// `cargo test` runs all of a crate's tests in one process by default.
///
/// The lock is susi-core's `commit_log::ENV_LOCK`: `cluster_key()` resolves
/// through the same env vars, so commit-ledger tests' seal→verify sequences
/// must serialize against env mutation too — a private lock would leave
/// that interleaving unprotected.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod slim_core_tests {
    #[test]
    fn slim_core_keeps_candle_behind_local_engine() {
        let gemi = include_str!("../../Cargo.toml");
        assert!(
            !gemi.contains("susi-vendor-candle"),
            "susi-gemi must not depend on susi-vendor-candle (moved to susi-gpu-fabric)"
        );
        assert!(
            gemi.contains("local-engine = [\"susi-gemi-models/local-engine\"]"),
            "local-engine feature remains forwarded to susi-gemi-models"
        );
        let features = gemi
            .split("[features]")
            .nth(1)
            .unwrap_or("")
            .split("[lints]")
            .next()
            .unwrap_or("");
        assert!(
            features.contains("default = []"),
            "susi-gemi must not enable local-engine by default"
        );
        assert!(
            !features.contains("default = [\"local-engine\"]"),
            "local-engine is opt-in"
        );

        let root = include_str!("../../../../Cargo.toml");
        let root_features = root
            .split("[features]")
            .nth(1)
            .unwrap_or("")
            .split("[profile.release]")
            .next()
            .unwrap_or("");
        assert!(
            root_features.contains("default = []"),
            "the susi package default build is the slim socket"
        );
        assert!(
            root_features.contains(
                "local-engine = [\"susi-gemi/local-engine\", \"susi-daemon/local-engine\"]"
            ),
            "the root local-engine feature must reach the daemon and gemi"
        );
        assert!(
            !root_features.lines().any(|line| {
                line.trim_start().starts_with("default") && line.contains("local-engine")
            }),
            "local-engine must not be a default feature of the susi package"
        );
    }
}
