//! Agent2Agent 1.0 protocol discovery and binding constants.
//! Full A2A wire schemas, codecs, routing and task storage live in Susi Commercial Apps.

/// The A2A protocol revision the card advertises.
pub const PROTOCOL_VERSION: &str = "1.0";

/// The JSON-RPC binding's name on an agent interface.
pub const JSONRPC_BINDING: &str = "JSONRPC";

/// Public A2A discovery path.
pub const WELL_KNOWN_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
