//! `clap` definitions for the `patch` command family, moved verbatim from
//! `src/cli/patch_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/patch_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.

#[derive(Debug, clap::Args)]
#[allow(missing_docs)] // clap uses doc comments as user-facing --help; undocumented fields stay undocumented to preserve the CLI help snapshot.
pub struct PatchApplyArgs {
    /// JSON file containing a PatchRequest.
    pub file: std::path::PathBuf,
    /// Override auto_apply (still requires workspace confinement).
    #[arg(long)]
    pub auto_apply: bool,
}
