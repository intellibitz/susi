//! Model-selection tier compiled into GEMI without a Cargo edge.

pub use crate::susi_sandbox;

#[path = "../../susi-gemi-models/src/catalog_store.rs"]
pub mod catalog_store;
#[path = "../../susi-gemi-models/src/cloud.rs"]
pub mod cloud;
#[path = "../../susi-gemi-models/src/coding_models.rs"]
pub mod coding_models;
#[path = "../../susi-gemi-models/src/download.rs"]
pub(crate) mod download;
#[path = "../../susi-gemi-models/src/frontier.rs"]
pub mod frontier;
#[path = "../../susi-gemi-models/src/hardware.rs"]
pub mod hardware;
#[path = "../../susi-gemi-models/src/hf_discovery.rs"]
pub mod hf_discovery;
#[path = "../../susi-gemi-models/src/intent.rs"]
pub mod intent;
#[path = "../../susi-gemi-models/src/lifecycle/mod.rs"]
mod lifecycle;
#[path = "../../susi-gemi-models/src/model_cache.rs"]
pub mod model_cache;
#[path = "../../susi-gemi-models/src/open_weight.rs"]
pub mod open_weight;
#[path = "../../susi-gemi-models/src/openrouter.rs"]
pub mod openrouter;

pub use lifecycle::*;
