//! Vendor facade for `parking_lot`. Consumers depend on this crate under the
//! dep key `parking_lot` (via `package = "susi-vendor-parking-lot"`), so `use parking_lot::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use parking_lot::*;
