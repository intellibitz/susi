#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! GEMI socket contract: its plane-bus topics and hardware profile. Concrete
//! inference engines, model operations and the typed call wrappers over these
//! topics are hosted by Susi Commercial Apps through the same plane bus.
pub use susi_core::plane_bus::{topics, HardwareProfileDto};
pub use susi_gemi_models as models;
