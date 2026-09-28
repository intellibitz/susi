//! Vendor facade for `criterion`. Consumers depend on this crate under the
//! dep key `criterion` (via `package = "susi-vendor-criterion"`), so `use criterion::...`
//! paths resolve unchanged.
#![forbid(unsafe_code)]

pub use criterion::*;
