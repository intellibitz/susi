//! Vendor facade for `signal-hook`. Consumers depend on this crate under the
//! dep key `signal-hook` (via `package = "susi-vendor-signal-hook"`), so `use signal_hook::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use signal_hook::*;
