//! Vendor facade for `anyhow`. Consumers depend on this crate under the
//! dep key `anyhow` (via `package = "susi-vendor-anyhow"`), so `use anyhow::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use anyhow::*;
