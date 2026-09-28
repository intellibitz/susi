//! Vendor facade for `hyper-util`. Consumers depend on this crate under the
//! dep key `hyper-util` (via `package = "susi-vendor-hyper-util"`), so `use hyper_util::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use hyper_util::*;
