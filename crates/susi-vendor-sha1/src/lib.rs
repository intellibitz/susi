//! Vendor facade for `sha1`. Consumers depend on this crate under the
//! dep key `sha1` (via `package = "susi-vendor-sha1"`), so `use sha1::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use sha1::*;
