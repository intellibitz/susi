//! Vendor facade for `rcgen`. Consumers depend on this crate under the
//! dep key `rcgen` (via `package = "susi-vendor-rcgen"`), so `use rcgen::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use rcgen::*;
