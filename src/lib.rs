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
// The composition root depends on the real leaf crates rather than
// vendored IPC clients — `susi::susi_error` stays reachable for API
// stability via this re-export.
pub use susi_error;

pub mod hooks;

/// Error types and handling
pub mod error {
    pub use susi_error::*;
}

/// Native execution primitives
pub mod native {
    pub use susi_vendor_wasmtime::*;
}

/// Sandbox and workspace management
pub mod sandbox {
    pub use susi_sandbox_client::*;
    /// XDG directory specifications
    pub mod xdg {
        pub use susi_paths::SusiDirs;
    }
}

/// Current version of susi
pub const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn susi_version_is_non_empty_semver() {
        assert!(!SUSI_VERSION.is_empty());
        assert_eq!(SUSI_VERSION.split('.').count(), 3);
    }

    #[test]
    fn error_reexport_constructs_and_formats_cleanly() {
        let err = error::EaiError::governance("invalid key");
        assert!(err.to_string().contains("invalid key"));
    }

    #[test]
    fn sandbox_xdg_susi_dirs_resolves_paths() {
        let home = sandbox::xdg::SusiDirs::substrate_home();
        assert!(!home.as_os_str().is_empty());
    }

    #[test]
    fn hooks_module_reexports_engine_hooks() {
        let _hooks = hooks::SusiEngineHooks;
        assert_eq!(std::mem::size_of::<hooks::SusiEngineHooks>(), 0);
    }
}
