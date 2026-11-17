#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Susi's public launcher and generic protocol contracts. Concrete command,
//! daemon, agent, model and tool implementations run in the commercial runtime.
//!
//! - [`error`]: shared typed errors
//! - [`native`]: native/WASM execution primitives
//! - [`sandbox`]: generic workspace paths

#![warn(missing_docs)]

// The composition root depends on the real leaf crates rather than
// vendored IPC clients — `susi::susi_error` stays reachable for API
// stability via this re-export.
pub use susi_error;

#[cfg(windows)]
#[doc(hidden)] // The installed channel's own launch bindings, not a public API.
pub mod channel_bindings;

/// Error types and handling
pub mod error {
    pub use susi_error::*;
}

/// Native execution primitives
pub mod native {
    pub use susi_vendor_wasmtime::*;
}

/// Generic workspace paths. Concrete sandbox management is served by the commercial runtime.
pub mod sandbox {
    /// XDG directory specifications
    pub mod xdg {
        pub use susi_paths::SusiDirs;
    }
}

/// Current version of susi
pub const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn susi_version_is_non_empty_semver() {
        assert!(!SUSI_VERSION.is_empty());
        assert_eq!(SUSI_VERSION.split('.').count(), 3);
    }

    #[test]
    fn error_reexport_constructs_and_formats_cleanly() {
        let err = error::EaiError::governance("invalid key");
        assert!(err.to_string().contains("invalid key"));
    }

    #[test]
    fn sandbox_xdg_susi_dirs_resolves_paths() {
        let home = sandbox::xdg::SusiDirs::substrate_home();
        assert!(!home.as_os_str().is_empty());
    }
}
