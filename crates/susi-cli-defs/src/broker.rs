//! `clap` definitions for the `broker` command family, moved verbatim from
//! `src/cli/broker_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/broker_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi broker`: inter-app permission and message broker.
#[derive(Debug, Subcommand)]
pub enum BrokerCommands {
    /// Grant a permission scope to an identity.
    #[allow(missing_docs)]
    Grant {
        // Long-only: `-g` belongs to `--grantee`, matching `check`, `revoke`
        // and `list`. Sharing `-g` panicked clap's debug asserts.
        /// Identity issuing the grant.
        #[arg(long)]
        grantor: String,
        #[arg(short, long)]
        grantee: String,
        #[arg(short, long)]
        resource: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long)]
        ttl_secs: Option<u64>,
    },
    /// Request a permission (opens a negotiation).
    #[allow(missing_docs)]
    Request {
        #[arg(short = 'r', long)]
        requester: String,
        #[arg(short = 'o', long, default_value = "susi")]
        grantor: String,
        #[arg(short = 'R', long)]
        resource: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long)]
        ttl_secs: Option<u64>,
    },
    /// Resolve a pending permission request.
    #[allow(missing_docs)]
    Negotiate {
        #[arg(short, long)]
        request_id: String,
        #[arg(short, long)]
        actor: String,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        deny: bool,
    },
    /// List pending permission requests.
    #[allow(missing_docs)]
    Pending {
        #[arg(short, long)]
        grantor: Option<String>,
    },
    /// Check whether an identity holds a scope.
    #[allow(missing_docs)]
    Check {
        #[arg(short, long)]
        grantee: String,
        #[arg(short, long)]
        resource: String,
        #[arg(short, long)]
        action: String,
    },
    /// Revoke a permission scope from an identity.
    #[allow(missing_docs)]
    Revoke {
        #[arg(short, long)]
        grantee: String,
        #[arg(short, long)]
        resource: String,
        #[arg(short, long)]
        action: String,
    },
    /// List grants for an identity.
    #[allow(missing_docs)]
    List {
        #[arg(short, long)]
        grantee: String,
    },
    /// Send a message to another identity (requires dispatch grant).
    #[allow(missing_docs)]
    Send {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        to: String,
        // Long-only: `-t` belongs to `--to`; sharing it panicked clap.
        /// Message topic.
        #[arg(long)]
        topic: String,
        /// JSON payload string.
        #[arg(short, long)]
        payload: String,
    },
    /// Receive messages for an identity.
    #[allow(missing_docs)]
    Receive {
        #[arg(short, long)]
        recipient: String,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
}
