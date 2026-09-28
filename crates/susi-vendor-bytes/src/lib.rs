//! Vendor facade for `bytes`. Consumers depend on this crate under the
//! dep key `bytes` (via `package = "susi-vendor-bytes"`), so `use bytes::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use bytes::*;
