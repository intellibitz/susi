//! Vendor facade for `ed25519-dalek`. Consumers depend on this crate under the
//! dep key `ed25519-dalek` (via `package = "susi-vendor-ed25519-dalek"`), so `use ed25519_dalek::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use ed25519_dalek::*;
