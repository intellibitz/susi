//! Vendor facade for `flume`. Consumers depend on this crate under the
//! dep key `flume` (via `package = "susi-vendor-flume"`), so `use flume::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use flume::*;
