//! Vendor facade for `getrandom`. Consumers depend on this crate under the
//! dep key `getrandom` (via `package = "susi-vendor-getrandom"`), so `use getrandom::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use getrandom::*;
