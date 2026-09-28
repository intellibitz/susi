//! Vendor facade for `ureq`. Consumers depend on this crate under the
//! dep key `ureq` (via `package = "susi-vendor-ureq"`), so `use ureq::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use ureq::*;
