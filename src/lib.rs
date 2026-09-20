#![warn(missing_docs)]
pub mod hooks;
// susi-engine: a local-first Rust engine for running and orchestrating AI
// models. Loads model weights directly (.safetensors, GGUF, ONNX via
// Candle), queues work through a lock-free async intent pipeline, and can
// fan work out across multiple agents running in parallel. Exposes tools
// over the Model Context Protocol (MCP).
//
// - [`daemon`]: background process management and lifecycle
// - [`gawd`]: agent orchestration and reflex synthesis
// - [`gemi`]: model loading and inference (Candle-based)
// - [`gmcp`]: Model Context Protocol server/client and tool registry
// - [`native`]: native/WASM execution primitives
// - [`sandbox`]: workspace and config management

pub mod error {
    pub use susi_error::*;
}
pub mod native {
    pub use susi_native::*;
}
pub mod sandbox {
    pub use susi_sandbox::*;
    pub mod xdg {
        pub use susi_paths::SusiDirs;
    }
}

pub const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");
