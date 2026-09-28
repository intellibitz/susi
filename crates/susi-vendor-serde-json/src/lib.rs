//! Vendor facade for `serde_json`. Consumers depend on this crate under the
//! dep key `serde_json` (via `package = "susi-vendor-serde-json"`), so `use serde_json::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use serde_json::*;
