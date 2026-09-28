//! Vendor facade for `serde`. Consumers depend on this crate under the
//! dep key `serde` (via `package = "susi-vendor-serde"`), so `use serde::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use serde::*;
