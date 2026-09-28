//! Vendor facade for `sysinfo`. Consumers depend on this crate under the
//! dep key `sysinfo` (via `package = "susi-vendor-sysinfo"`), so `use sysinfo::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use sysinfo::*;
