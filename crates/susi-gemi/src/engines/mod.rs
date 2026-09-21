//! Inference **engines** tier: backends, protocol providers, and routing.
//!
//! This tier *runs* tokens. It may call [`crate::models`] (`susi-gemi-models`)
//! for path resolution, readiness, and selection — it owns Candle/HTTP/MCP
//! execution graphs.
//!
//! Prefer `susi_gemi::engines::…` for new code; flat `susi_gemi::engine` /
//! `susi_gemi::http_provider` paths remain as compatibility re-exports.

pub mod alpha;
pub mod audio;
pub mod candle_provider;
pub mod http_provider;
pub mod mcp_provider;
pub mod qwen2_split;
pub mod reasoning;
pub mod reflex;
pub mod routing;
/// Core local/cloud inference runtime (`GemiEngine`, `NeuralBackend`, …).
pub mod runtime;
pub mod speculative;
pub(crate) mod token_stream;
pub mod unified;
pub mod vision;
pub mod vllm;
