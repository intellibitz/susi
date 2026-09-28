//! Vendor facade for `dashmap`. Consumers depend on this crate under the
//! dep key `dashmap` (via `package = "susi-vendor-dashmap"`), so `use dashmap::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use dashmap::*;
