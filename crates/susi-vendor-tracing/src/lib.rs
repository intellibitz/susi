//! Vendor facade for `tracing`. Consumers depend on this crate under the
//! dep key `tracing` (via `package = "susi-vendor-tracing"`), so `use tracing::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tracing::*;
