//! ACP server entry point for susi-gemi
//!
//! Provides the main server implementation that integrates with the ACP SDK
//! to expose susi-gemi as a complete ACP-compliant agent.

use agent_client_protocol::schema::v1::{
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SetSessionModeRequest,
    SetSessionModeResponse,
};
use std::path::PathBuf;

use super::agent::GemiAgent;

/// ACP server for susi-gemi
pub struct GemiAcpServer {
    agent: GemiAgent,
}

impl GemiAcpServer {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            agent: GemiAgent::new(workspace),
        }
    }

    /// Run the ACP server using stdio transport
    pub async fn run_stdio(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("susi-gemi ACP server starting on stdio...");

        // This will be integrated with the ACP SDK's stdio server infrastructure
        // The actual implementation would use agent_client_protocol::Server with stdio transport

        Ok(())
    }

    /// Run the ACP server using HTTP transport
    pub async fn run_http(&self, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
        println!("susi-gemi ACP server starting on HTTP at {}...", addr);

        // This will be integrated with the ACP SDK's HTTP server infrastructure
        // using agent-client-protocol-http with server feature

        Ok(())
    }

    /// Handle initialize request
    pub async fn handle_initialize(&self, request: InitializeRequest) -> InitializeResponse {
        self.agent.handle_initialize(request).await
    }

    /// Handle session/new request
    pub async fn handle_session_new(&self, request: NewSessionRequest) -> NewSessionResponse {
        self.agent.handle_session_new(request).await
    }

    /// Handle session/load request
    pub async fn handle_session_load(&self, request: LoadSessionRequest) -> LoadSessionResponse {
        self.agent.handle_session_load(request).await
    }

    /// Handle session/prompt request
    pub async fn handle_session_prompt(&self, request: PromptRequest) -> PromptResponse {
        self.agent.handle_session_prompt(request).await
    }

    /// Handle session/set_mode request
    pub async fn handle_session_set_mode(
        &self,
        request: SetSessionModeRequest,
    ) -> SetSessionModeResponse {
        self.agent.handle_session_set_mode(request).await
    }
}

/// Create and run a new ACP server on stdio
pub async fn run_acp_server_stdio(workspace: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let server = GemiAcpServer::new(workspace);
    server.run_stdio().await
}

/// Create and run a new ACP server on HTTP
pub async fn run_acp_server_http(
    workspace: PathBuf,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = GemiAcpServer::new(workspace);
    server.run_http(addr).await
}
