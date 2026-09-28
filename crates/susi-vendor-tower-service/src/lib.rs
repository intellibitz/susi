//! Vendor facade for `tower-service`. Consumers depend on this crate under the
//! dep key `tower-service` (via `package = "susi-vendor-tower-service"`), so `use tower_service::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tower_service::*;
