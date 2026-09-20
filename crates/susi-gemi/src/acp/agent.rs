//! ACP agent implementation for susi-gemi
//!
//! Implements the ACP Agent trait to provide a complete ACP-compliant
//! agent that integrates with susi-gemi's inference engine.

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SetSessionModeRequest,
    SetSessionModeResponse,
};
use std::path::PathBuf;

use super::capabilities::GemiCapabilities;
use super::session::SessionManager;
use super::streaming::StreamingHandler;

/// susi-gemi ACP agent implementation
pub struct GemiAgent {
    workspace: PathBuf,
    session_manager: SessionManager,
    capabilities: GemiCapabilities,
    streaming_handler: StreamingHandler,
}

impl GemiAgent {
    pub fn new(workspace: PathBuf) -> Self {
        let session_manager = SessionManager::new(workspace.clone());
        let capabilities = GemiCapabilities::local_first();
        let streaming_handler = StreamingHandler::new(session_manager.clone(), workspace.clone());

        Self {
            workspace,
            session_manager,
            capabilities,
            streaming_handler,
        }
    }

    /// Handle initialize request
    pub async fn handle_initialize(&self, request: InitializeRequest) -> InitializeResponse {
        InitializeResponse::new(request.protocol_version)
            .agent_capabilities(self.capabilities.as_agent_capabilities())
            .auth_methods(vec![])
            .agent_info(
                agent_client_protocol::schema::v1::Implementation::new(
                    "susi-gemi",
                    env!("CARGO_PKG_VERSION"),
                )
                .title("SUSI GEMI AI Engine"),
            )
    }

    /// Handle session/new request
    pub async fn handle_session_new(&self, _request: NewSessionRequest) -> NewSessionResponse {
        let session_id = self.session_manager.create_session(None);
        NewSessionResponse::new(session_id)
    }

    /// Handle session/load request
    pub async fn handle_session_load(&self, _request: LoadSessionRequest) -> LoadSessionResponse {
        LoadSessionResponse::new()
    }

    /// Handle session/prompt request
    pub async fn handle_session_prompt(&self, request: PromptRequest) -> PromptResponse {
        // Extract text from prompt content blocks
        let prompt_text = request
            .prompt
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text(text_content) = block {
                    Some(text_content.text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let session_id_str = request.session_id.to_string();

        // Add user message to session
        self.session_manager
            .add_message(&session_id_str, "user".to_string(), prompt_text.clone());

        // Start streaming response
        let (_content_chunks, stop_reason) = self
            .streaming_handler
            .stream_response(&session_id_str, &prompt_text)
            .await;

        // Note: In a full implementation, content would be sent via session/update notifications
        // For now, we'll return the stop reason only
        PromptResponse::new(stop_reason)
    }

    /// Handle session/set_mode request
    pub async fn handle_session_set_mode(
        &self,
        request: SetSessionModeRequest,
    ) -> SetSessionModeResponse {
        let session_id_str = request.session_id.to_string();
        let mode_id_str = request.mode_id.to_string();
        self.session_manager.set_mode(&session_id_str, mode_id_str);
        SetSessionModeResponse::new()
    }
}
