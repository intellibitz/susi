//! `clap` definitions for the `privacy` command family, moved verbatim from
//! `src/cli/privacy_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/privacy_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi privacy`: privacy mode and MAC capability grants.
#[derive(Debug, Subcommand)]
pub enum PrivacyCommands {
    /// Show privacy mode and active capability grants.
    Status,
    /// Set privacy mode: balanced | local_only | open
    #[allow(missing_docs)]
    Mode { mode: String },
    /// Grant a cryptographic capability token.
    #[allow(missing_docs)]
    Grant {
        #[arg(short, long, default_value = "susi")]
        subject: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long, default_value = "*")]
        resource: String,
        #[arg(short, long)]
        ttl_secs: Option<u64>,
    },
    /// Revoke a capability grant.
    #[allow(missing_docs)]
    Revoke {
        #[arg(short, long, default_value = "susi")]
        subject: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long, default_value = "*")]
        resource: String,
    },
    /// Explicit consent for network egress (and cloud inference under local_only).
    #[allow(missing_docs)]
    Consent {
        #[arg(long)]
        egress: bool,
        #[arg(short, long, default_value = "susi")]
        subject: String,
        #[arg(short, long, default_value_t = 3600)]
        ttl_secs: u64,
    },
}
