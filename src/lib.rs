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
pub mod hooks;

/// Error types and handling
pub mod error {
    pub use susi_error::*;
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
        pub use susi_paths::SusiDirs;
    }
}

/// Current version of susi
pub const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");
