//! Engine hooks re-export — implementation lives in `susi-daemon`, the
//! composition-root crate that sees gawd + gemi + gmcp.

#![warn(missing_docs)]

pub use susi_daemon::SusiEngineHooks;
