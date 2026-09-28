//! Vendor facade for `clap`. Consumers depend on this crate under the
//! dep key `clap` (via `package = "susi-vendor-clap"`), so `use clap::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use clap::*;
