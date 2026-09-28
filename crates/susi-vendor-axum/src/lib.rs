//! Vendor facade for `axum`. Consumers depend on this crate under the
//! dep key `axum` (via `package = "susi-vendor-axum"`), so `use axum::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use axum::*;
