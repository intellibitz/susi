#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! GAWD socket contract: its plane-bus topics and the A2A and ACP protocol
//! crates. Execution, orchestration, policy and the typed mission and audit
//! call wrappers live in their host.
pub use susi_core::plane_bus::topics;
pub use susi_gawd_a2a as a2a;
pub use susi_gawd_acp as acp;
