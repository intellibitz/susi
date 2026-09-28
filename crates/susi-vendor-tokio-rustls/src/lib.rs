//! Vendor facade for `tokio-rustls`. Consumers depend on this crate under the
//! dep key `tokio-rustls` (via `package = "susi-vendor-tokio-rustls"`), so `use tokio_rustls::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tokio_rustls::*;
