//! Vendor facade for `tokio-stream`. Consumers depend on this crate under the
//! dep key `tokio-stream` (via `package = "susi-vendor-tokio-stream"`), so `use tokio_stream::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tokio_stream::*;
