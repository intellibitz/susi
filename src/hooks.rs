//! Engine hooks re-export — implementation lives in `susi-gmcp` so the daemon
//! composition root can wire hooks without depending on the root package.

#![warn(missing_docs)]

pub use susi_gmcp::engine_hooks::SusiEngineHooks;
