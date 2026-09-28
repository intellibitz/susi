//! Vendor facade for `tower`. Consumers depend on this crate under the
//! dep key `tower` (via `package = "susi-vendor-tower"`), so `use tower::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tower::*;
