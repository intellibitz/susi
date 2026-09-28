//! Vendor facade for `ra2a`. Consumers depend on this crate under the
//! dep key `ra2a` (via `package = "susi-vendor-ra2a"`), so `use ra2a::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use ra2a::*;
