//! `clap` definitions for the `acp` command family (5th Open-Core Pillar:
//! Agent Client Protocol).
use clap::Subcommand;

/// Subcommands of `susi acp`: native ACP JSON-RPC 2.0 stdio server and host discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum AcpCommands {
    /// Run SUSI's native ACP JSON-RPC 2.0 stdio server (default when bare `susi acp`)
    Serve,
    /// Print the host's canonical ACP discovery document (`susi-acp-discovery/v1`)
    Discover {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::AcpCommands;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: Option<AcpCommands>,
    }

    #[test]
    fn bare_acp_serve_and_discover_parse_cleanly() {
        let bare = TestCli::try_parse_from(["acp"]).unwrap();
        assert_eq!(bare.action, None);

        let serve = TestCli::try_parse_from(["acp", "serve"]).unwrap();
        assert_eq!(serve.action, Some(AcpCommands::Serve));

        let discover = TestCli::try_parse_from(["acp", "discover"]).unwrap();
        assert_eq!(discover.action, Some(AcpCommands::Discover { json: false }));

        let discover_json = TestCli::try_parse_from(["acp", "discover", "--json"]).unwrap();
        assert_eq!(
            discover_json.action,
            Some(AcpCommands::Discover { json: true })
        );

        assert!(TestCli::try_parse_from(["acp", "unknown"]).is_err());
    }
}
