//! Vendor facade for `winapi`. Consumers depend on this crate under the
//! dep key `winapi` (via `package = "susi-vendor-winapi"`), so `use winapi::...`
//! paths resolve unchanged while the third-party crate is linked exactly
//! once, here. See AGENTS.md "Multi-agent parallel work" / zero-external-deps.
#![forbid(unsafe_code)]

// winapi exports no items off Windows; the facade still owns the dep so
// Windows-only consumers keep `use winapi::...` under a zero-external graph.
#[allow(unused_imports)]
pub use winapi::*;
