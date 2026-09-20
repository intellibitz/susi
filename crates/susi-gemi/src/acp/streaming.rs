//! ACP streaming implementation for susi-gemi
//!
//! Handles streaming responses with session/update notifications as required
//! by the ACP protocol.

use super::session::SessionManager;
use std::path::PathBuf;

/// Streaming handler for ACP session/update notifications
pub struct StreamingHandler {
    #[allow(dead_code)]
    session_manager: SessionManager,
    workspace: PathBuf,
}

impl StreamingHandler {
    pub fn new(session_manager: SessionManager, workspace: PathBuf) -> Self {
        Self {
            session_manager,
            workspace,
        }
    }

    /// Stream a response with session/update notifications
    pub async fn stream_response(
        &self,
        _session_id: &str,
        prompt: &str,
    ) -> (Vec<String>, agent_client_protocol::schema::v1::StopReason) {
        // Use the susi-gemi inference engine via GemiEngine
        let response = self.generate_with_gemi_engine(prompt).await;
        let chunks = self.chunk_response(&response);

        // In a real implementation, we would send session/update notifications
        // for each chunk through the ACP notification mechanism

        (
            chunks,
            agent_client_protocol::schema::v1::StopReason::EndTurn,
        )
    }

    /// Generate response using susi-gemi inference engine
    async fn generate_with_gemi_engine(&self, prompt: &str) -> String {
        // Integrate with the actual susi-gemi GemiEngine
        // This will call the inference engine to generate responses
        crate::engine::GemiEngine::generate_reasoning(prompt, &self.workspace)
    }

    /// Split response into chunks for streaming
    fn chunk_response(&self, response: &str) -> Vec<String> {
        // Simple chunking strategy - can be made more sophisticated
        let chunk_size = 100;
        response
            .chars()
            .collect::<Vec<char>>()
            .chunks(chunk_size)
            .map(|chunk| chunk.iter().collect())
            .collect()
    }
}
