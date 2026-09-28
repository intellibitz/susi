//! Vendor facade for `hyper`. Consumers depend on this crate under the
//! dep key `hyper` (via `package = "susi-vendor-hyper"`), so `use hyper::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use hyper::*;
