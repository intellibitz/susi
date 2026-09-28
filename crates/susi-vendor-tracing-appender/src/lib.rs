//! Vendor facade for `tracing-appender`. Consumers depend on this crate under the
//! dep key `tracing-appender` (via `package = "susi-vendor-tracing-appender"`), so `use tracing_appender::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use tracing_appender::*;
