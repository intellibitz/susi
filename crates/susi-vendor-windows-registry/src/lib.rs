//! Thin facade for native Windows registry APIs. The upstream Rust binding is
//! the only implementation; this crate adds no processes, data or runtime state.
#![forbid(unsafe_code)]

#[cfg(windows)]
pub use windows_registry::*;
