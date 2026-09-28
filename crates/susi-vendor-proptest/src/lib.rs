//! Vendor facade for `proptest`. Consumers depend on this crate under the
//! dep key `proptest` (via `package = "susi-vendor-proptest"`), so `use proptest::...`
//! paths resolve unchanged.
#![forbid(unsafe_code)]

pub use proptest::*;
