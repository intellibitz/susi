//! Vendor facade for `hex`. Consumers depend on this crate under the
//! dep key `hex` (via `package = "susi-vendor-hex"`), so `use hex::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use hex::*;
