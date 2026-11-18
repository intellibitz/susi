//! `clap` definitions for the `extensions` command family, moved verbatim from
//! `src/cli/extensions_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/extensions_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi extensions`: manage extension packs.
#[derive(Debug, Subcommand)]
pub enum ExtensionCommands {
    /// List installed packs (default when bare `susi extensions`)
    List,
    /// Show active pack and substrate root
    Status,
    /// Create an empty pack shell under ~/.susi/extensions/<id>/
    #[allow(missing_docs)]
    Create { id: String },
    /// Load (activate) a pack
    #[allow(missing_docs)]
    Load { id: String },
    /// Unload a pack (keeps files; falls back to compiled defaults)
    #[allow(missing_docs)]
    Unload { id: String },
    /// Check a pack directory against the host's admission rules (for pack
    /// authors; reads and changes no host state)
    #[allow(missing_docs)]
    Validate { path: std::path::PathBuf },
}
