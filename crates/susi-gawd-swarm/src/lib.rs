#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Swarm socket contract: the plane-bus topics cluster discovery and
//! cancellable missions travel on. Swarm orchestration and its typed call
//! wrappers are owned by the separately hosted implementation.
pub use susi_core::plane_bus::topics;
