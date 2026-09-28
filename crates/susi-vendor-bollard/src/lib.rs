//! Vendor facade for `bollard`. Consumers depend on this crate under the
//! dep key `bollard` (via `package = "susi-vendor-bollard"`), so `use bollard::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use bollard::*;
