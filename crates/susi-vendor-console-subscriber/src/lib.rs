//! Vendor facade for `console-subscriber`. Consumers depend on this crate under the
//! dep key `console-subscriber` (via `package = "susi-vendor-console-subscriber"`), so `use console_subscriber::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use console_subscriber::*;
