//! Vendor facade for `chacha20poly1305`. Consumers depend on this crate under the
//! dep key `chacha20poly1305` (via `package = "susi-vendor-chacha20poly1305"`), so `use chacha20poly1305::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use chacha20poly1305::*;
