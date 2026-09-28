//! Vendor facade for `libc`. Consumers depend on this crate under the
//! dep key `libc` (via `package = "susi-vendor-libc"`), so `use libc::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use libc::*;
