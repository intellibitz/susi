#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Harness control and event wire contracts with generic socket dispatch.
//! Budget state, scheduling, workers and provider routing live in their host.
pub mod api;
pub mod event;
pub mod money;
pub use susi_core::plane_bus::daemon::harness as dispatch;
