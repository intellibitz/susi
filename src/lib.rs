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

//! susi: a local-first Rust substrate for orchestrating AI agents and models.
//! Local Candle inference loads llama/qwen2 **GGUF** weights; remote models
//! mount via OpenAI-compatible (and Anthropic/Gemini/Triton) engines. Work
//! queues through a lock-free async intent pipeline and fans out across a
//! multi-agent swarm. Tools expose over the Model Context Protocol (MCP).
//!
//! - [`daemon`]: background process management and lifecycle
//! - [`gawd`]: agent orchestration and reflex synthesis
//! - [`gemi`]: model loading and inference (Candle-based)
//! - [`gmcp`]: Model Context Protocol server/client and tool registry
//! - [`native`]: native/WASM execution primitives
//! - [`sandbox`]: workspace and config management

#![warn(missing_docs)]

/// Engine hooks for extension points
// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// missing_docs is waived because the file is vendored byte-identical —
// doc additions here would diverge it from every other crate's copy.
#[allow(dead_code, missing_docs)]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
mod susi_paths;

pub mod hooks;

/// Error types and handling
pub mod error {
    pub use crate::susi_error::*;
}

/// Native execution primitives
pub mod native {
    pub use susi_native::*;
}

/// Sandbox and workspace management
pub mod sandbox {
    pub use susi_sandbox::*;
    /// XDG directory specifications
    pub mod xdg {
        pub use crate::susi_paths::SusiDirs;
    }
}

/// Current version of susi
pub const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");
