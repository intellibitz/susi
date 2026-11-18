#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic GAWD mission and audit socket bindings.
//! Execution, orchestration and policy implementations live in their host.
pub use susi_core::plane_bus::gawd::*;
pub use susi_core::plane_bus::{gawd_hooks, topics};
pub use susi_gawd_a2a as a2a;
pub use susi_gawd_acp as acp;
