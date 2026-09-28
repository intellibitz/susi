//! Vendor facade for `http-body-util`. Consumers depend on this crate under the
//! dep key `http-body-util` (via `package = "susi-vendor-http-body-util"`), so `use http_body_util::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use http_body_util::*;
