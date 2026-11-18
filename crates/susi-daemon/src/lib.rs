#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic host-control socket bindings.
//! Process ownership and runtime composition live with the hosted daemon.
pub use susi_core::plane_bus::daemon::*;
