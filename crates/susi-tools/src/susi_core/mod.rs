//! Vendored `susi_core` subset — canonical copies live in
//! `crates/susi-core/vendor_template/susi_core/`.
//!
//! Same public API as the `susi-core` crate, but the process-wide singletons
//! are backed by the `<cache>/bus/<pid>/` rendezvous (`plane_bus_ipc`),
//! file-backed state (`broker`, `mac_policy` grants/mode), and a receipts
//! inbox (`capture`), so independent vendored copies in one process
//! interoperate with each other and with the daemon's real `susi_core`.
//! Leaf modules (`susi_error`, `susi_paths`, `susi_config`, `susi_sandbox`,
//! `susi_native`) are referenced at the consumer's crate root — vendor them
//! first.

pub mod agent_types;
pub mod broker;
pub mod capture;
pub mod context_graph;
pub mod evidence;
pub mod mac_policy;
pub mod net_guard;
pub mod plane_bus;
pub mod plane_bus_ipc;
pub mod provider;
pub mod receipt_archive;
pub mod registry;
pub mod registry_ipc;
pub mod telemetry;

// Leaf modules vendored at the consumer's crate root, re-exported so
// `susi_core::susi_error::…` / `susi_core::redact::…` call sites resolve
// unchanged against the local vendored module.
pub use crate::susi_error;
pub use crate::susi_error::redact;

pub use telemetry::TelemetrySnapshot;
