#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic GEMI socket bindings. Concrete inference engines and model
//! operations are hosted by Susi Commercial Apps through the same plane bus.
pub use susi_core::plane_bus::gemi::*;
pub use susi_core::plane_bus::{topics, HardwareProfileDto};
pub use susi_gemi_models as models;
