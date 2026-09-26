//! Inference **engines** tier: backends, protocol providers, and routing.
//!
//! This tier *runs* tokens. It may call [`crate::models`] (`susi-gemi-models`)
//! for path resolution, readiness, and selection — it owns Candle/HTTP/MCP
//! execution graphs.
//!
//! Prefer `susi_gemi::engines::…` for new code; flat `susi_gemi::engine` /
//! `susi_gemi::http_provider` paths remain as compatibility re-exports.

pub mod alpha;
pub(crate) mod candle_err;
pub mod candle_provider;
pub mod http_provider;
pub mod mcp_provider;
pub mod qwen2_split;
pub mod reasoning;
pub mod reflex;
pub mod reflex_llm;
pub mod routing;
/// Core local/cloud inference runtime (`GemiEngine`, `NeuralBackend`, …).
pub mod runtime;
pub mod speculative;
pub(crate) mod token_stream;

/// Serializes every test in this crate that mutates the process-global
/// `HOME`/`XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`SUSI_XDG` env vars, or that
/// reads/writes a real file under `crate::susi_paths::SusiDirs::config_dir()`
/// (which resolves through those same env vars). Env var mutation is
/// visible to every thread immediately, so two tests in different files
/// that each use their own private lock (as `routing.rs` and
/// `http_provider.rs` used to) can still race against each other, since
/// `cargo test` runs all of a crate's tests in one process by default.
///
/// The lock is the vendored `commit_log::ENV_LOCK`: `cluster_key()` resolves
/// through the same env vars, so commit-ledger tests' seal→verify sequences
/// must serialize against env mutation too — a private lock would leave
/// that interleaving unprotected.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
