//! Vendor facade for `url`. Consumers depend on this crate under the
//! dep key `url` (via `package = "susi-vendor-url"`), so `use url::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use url::*;
