//! Vendor facade for `md-5`. Consumers depend on this crate under the
//! dep key `md-5` (via `package = "susi-vendor-md-5"`), so `use md_5::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use md5::*;
