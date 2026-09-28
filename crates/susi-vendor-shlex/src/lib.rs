//! Vendor facade for `shlex`. Consumers depend on this crate under the
//! dep key `shlex` (via `package = "susi-vendor-shlex"`), so `use shlex::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

pub use shlex::*;
