#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic cluster discovery and cancellable mission socket bindings.
//! Swarm orchestration is owned by the separately hosted implementation.
pub use susi_core::plane_bus::gawd::{
    cluster_peers, cluster_roster, solve_mission_cancellable, solve_mission_for_acp,
    solve_mission_generative, solve_mission_generative_with_chat, solve_mission_with_model,
};
