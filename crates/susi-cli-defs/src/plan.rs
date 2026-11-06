//! `clap` definitions for the `plan` command family, moved verbatim from
//! `src/cli/plan_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/plan_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.

#[derive(Debug, clap::Args)]
#[allow(missing_docs)] // clap uses doc comments as user-facing --help; undocumented fields stay undocumented to preserve the CLI help snapshot.
pub struct PlanArgs {
    /// Natural language goal to solve via iterative planning.
    pub goal: String,
    /// Maximum number of planning steps.
    #[arg(short, long, default_value_t = 3)]
    pub max_steps: u32,
}
