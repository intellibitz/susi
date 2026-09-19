pub mod agents;
pub mod ama;
pub mod amas;
pub mod axiom;
pub mod bloat_audit;
pub mod brain;
pub mod dag;
pub mod genome_distiller;
pub mod kernel_loader;
pub mod net_guard;
pub mod pkb;
pub mod reason_trainer;
pub mod reflex_synth;
pub mod reflex_trainer;
pub mod safety;
pub mod security;
pub mod self_core;
pub mod task_manager;

pub use susi_core::{bus, evidence, manifold, queue, truth};

pub use ama::SusiMasterAgent;
