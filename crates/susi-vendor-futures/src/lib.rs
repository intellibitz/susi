//! Vendor facade for `futures`. Consumers depend on this crate under the
//! dep key `futures` (via `package = "susi-vendor-futures"`), so `use futures::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use futures::*;
