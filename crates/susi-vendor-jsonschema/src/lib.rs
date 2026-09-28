//! Vendor facade for `jsonschema`. Consumers depend on this crate under the
//! dep key `jsonschema` (via `package = "susi-vendor-jsonschema"`), so `use jsonschema::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use jsonschema::*;
