//! Vendor facade for `base64`. Consumers depend on this crate under the
//! dep key `base64` (via `package = "susi-vendor-base64"`), so `use base64::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use base64::*;
