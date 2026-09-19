pub mod agents;
pub mod ama;
pub mod amas;
pub mod axiom;
pub mod bloat_audit;
pub mod brain;
pub mod dag;
pub mod evolution;
pub mod admin;
pub mod genome_distiller;
pub mod kernel_loader;
pub mod pkb;
pub mod reason_trainer;
pub mod reflex_synth;
pub mod reflex_trainer;
pub mod safety;
pub mod security;
pub mod self_core;
pub mod self_validation;

pub use susi_agents::{net_guard, task_manager};
pub use susi_core::{bus, evidence, manifold, queue, truth};

pub use ama::SusiMasterAgent;
