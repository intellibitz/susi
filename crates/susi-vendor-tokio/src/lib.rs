//! Vendor facade for `tokio`. Consumers depend on this crate under the
//! dep key `tokio` (via `package = "susi-vendor-tokio"`), so `use tokio::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tokio::*;
