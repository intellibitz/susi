//! `clap` definitions for the `peers` command family, moved verbatim from
//! `src/cli/peers_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/peers_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi peers`: the verified cluster roster.
#[derive(Debug, Subcommand)]
pub enum PeersCommands {
    /// List verified peers and banned members (default)
    List {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
    },
    /// Bootstrap cluster membership: send a cluster-key-signed discovery
    /// ping to a host and persist the verified responder as an explicit
    /// member. Fails closed when the peer doesn't hold our cluster.key.
    Add {
        /// IP or hostname of the peer daemon to join (may include :port)
        host: String,
        /// UDP discovery port (default: host-contract 9092 + port_offset)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Probe a peer without persisting anything: the same signed-ping →
    /// signed-pong handshake `peers add` performs, reporting only the
    /// responder's attested identity and its standing in this node's
    /// roster. The non-destructive way to test reachability and
    /// cluster.key compatibility before (or instead of) `peers add`.
    Probe {
        /// IP or hostname of the peer daemon to probe (may include :port)
        host: String,
        /// UDP discovery port (default: host-contract 9092 + port_offset)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Evict a peer: revoke persisted trust AND ban re-verification.
    /// The ban is enforced at the signed-pong handshake — the peer cannot
    /// rejoin until `susi peers unban` lifts it.
    Remove {
        /// node_id or address prefix of the peer to evict
        peer: String,
    },
    /// Lift a ban so a previously evicted peer can re-verify
    Unban {
        /// node_id or address of the banned peer to restore
        peer: String,
    },
    /// Rotate the cluster key across every verified member — the only
    /// way to truly revoke an evicted member's cluster.key. Two-phase:
    /// a `cluster_rekey` record commits WHICH key members stage; a
    /// `cluster_rekey_activate` record then triggers rotation. If any
    /// member fails to stage, the rotation aborts safely (nothing
    /// activates anywhere) unless `--force` is given. Members that
    /// don't acknowledge activation are cryptographically stranded.
    Rekey {
        /// Proceed to activation even when some members failed to stage
        /// — those members will be stranded and must be re-provisioned
        /// with the new key manually.
        #[arg(long)]
        force: bool,
    },
    /// Cluster-wide consensus view: call `cluster_status` on every
    /// rostered member (member-signed requests) and print each node's
    /// term, leader view, ledger size, and key epoch side by side —
    /// divergence shows up directly as differing terms, epochs, or
    /// record counts.
    Status {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
    },
}
