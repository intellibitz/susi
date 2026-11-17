#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic model discovery and selection contracts for the GEMI socket.
//! Model catalogs, downloads and provider implementations live with their host.
pub use susi_core::plane_bus::gemi::{
    placement_constraints, valid_provider_id, validated_max_cost, validated_requirement,
    HardwareProfiler, ModelManager, PlacementConstraints,
};
pub use susi_core::plane_bus::HardwareProfileDto;
