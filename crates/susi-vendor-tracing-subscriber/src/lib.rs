//! Vendor facade for `tracing-subscriber`. Consumers depend on this crate under the
//! dep key `tracing-subscriber` (via `package = "susi-vendor-tracing-subscriber"`), so `use tracing_subscriber::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tracing_subscriber::*;
