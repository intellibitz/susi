#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Model discovery and selection contract for the GEMI socket: the hardware
//! profile a model is placed against. Model catalogs, downloads, provider
//! implementations and their typed call wrappers live with their host.
pub use susi_core::plane_bus::HardwareProfileDto;
