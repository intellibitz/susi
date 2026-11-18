#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! Agent2Agent protocol contracts. Serving and execution live with the implementation owner.
/// Agent Client Protocol contracts.
pub use susi_gawd_acp as acp;
pub mod protocol;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2a_and_acp_discovery_contracts_remain_pinned() {
        assert_eq!(protocol::PROTOCOL_VERSION, "1.0");
        assert_eq!(protocol::JSONRPC_BINDING, "JSONRPC");
        assert_eq!(
            protocol::WELL_KNOWN_AGENT_CARD_PATH,
            "/.well-known/agent-card.json"
        );
        assert_eq!(acp::ACP_CANONICAL_PORT, 9094);
    }
}
