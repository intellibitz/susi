//! Vendor facade for `regex`. Consumers depend on this crate under the
//! dep key `regex` (via `package = "susi-vendor-regex"`), so `use regex::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use regex::*;
