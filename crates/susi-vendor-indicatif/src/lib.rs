//! Vendor facade for `indicatif`. Consumers depend on this crate under the
//! dep key `indicatif` (via `package = "susi-vendor-indicatif"`), so `use indicatif::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use indicatif::*;
