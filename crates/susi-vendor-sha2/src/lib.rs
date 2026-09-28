//! Vendor facade for `sha2`. Consumers depend on this crate under the
//! dep key `sha2` (via `package = "susi-vendor-sha2"`), so `use sha2::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use sha2::*;
