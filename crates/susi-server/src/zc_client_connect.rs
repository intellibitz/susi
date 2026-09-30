//! `susi connect` wires MCP and HTTP clients to the host.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEndpoints {
    pub mcp_url: String,
    pub http_url: String,
    pub token_path: String,
}

/// Build client wiring from published ports and home.
#[must_use]
pub fn connect_endpoints(home: &str, mcp_port: u16, http_port: u16) -> ClientEndpoints {
    ClientEndpoints {
        mcp_url: format!("http://127.0.0.1:{mcp_port}/mcp"),
        http_url: format!("http://127.0.0.1:{http_port}"),
        token_path: format!("{home}/token"),
    }
}

#[cfg(test)]
mod zc_client_connect_tests {
    use super::*;

    #[test]
    fn zc_client_connect_wires_mcp_and_http() {
        let e = connect_endpoints("/tmp/.susi", 9090, 9091);
        assert!(e.mcp_url.contains("9090"));
        assert!(e.http_url.contains("9091"));
        assert!(e.token_path.ends_with("token"));
    }
}
