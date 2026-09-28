//! Vendor facade for `x25519-dalek`. Consumers depend on this crate under the
//! dep key `x25519-dalek` (via `package = "susi-vendor-x25519-dalek"`), so `use x25519_dalek::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use x25519_dalek::*;
