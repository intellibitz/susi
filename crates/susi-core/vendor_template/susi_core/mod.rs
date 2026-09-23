//! Vendored `susi_core` subset — canonical copies live in
//! `crates/susi-core/vendor_template/susi_core/`.
//!
//! Same public API as the `susi-core` crate, but the process-wide singletons
//! are backed by the `<cache>/bus/<pid>/` rendezvous (`plane_bus_ipc`) and
//! file-backed state (`broker`) so independent vendored copies in one
//! process interoperate. Leaf modules (`susi_error`, `susi_paths`,
//! `susi_config`, `susi_sandbox`, `susi_native`) are referenced at the
//! consumer's crate root — vendor them first.

pub mod agent_types;
pub mod broker;
pub mod context_graph;
pub mod net_guard;
pub mod plane_bus;
pub mod plane_bus_ipc;
pub mod telemetry;

pub use telemetry::TelemetrySnapshot;
