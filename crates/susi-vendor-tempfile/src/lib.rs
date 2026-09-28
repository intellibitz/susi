//! Vendor facade for `tempfile`. Consumers depend on this crate under the
//! dep key `tempfile` (via `package = "susi-vendor-tempfile"`), so `use tempfile::...`
//! paths resolve unchanged.
#![forbid(unsafe_code)]

pub use tempfile::*;
