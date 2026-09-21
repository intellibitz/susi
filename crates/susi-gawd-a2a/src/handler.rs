//! A2A protocol handler for susi-gawd
//!
//! Implements the A2A protocol handler for JSON-RPC and HTTP+JSON endpoints.

use super::executor::GawdA2AExecutor;
use ra2a::server::{a2a_router, HandlerBuilder};
use std::sync::Arc;

/// Create an A2A router for susi-gawd
pub fn create_a2a_router(agent_fleet: Arc<crate::agents::GawdAgentFleet>) -> axum::Router {
    let executor = GawdA2AExecutor::new(agent_fleet);
    let agent_card = executor.agent_card();
    
    let handler = HandlerBuilder::new(executor, agent_card)
        .build();
    
    a2a_router(handler)
}

/// A2A server configuration
pub struct A2AServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for A2AServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
        }
    }
}

/// Start the A2A server
pub async fn start_a2a_server(
    agent_fleet: Arc<crate::agents::GawdAgentFleet>,
    config: A2AServerConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = create_a2a_router(agent_fleet);
    
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", config.host, config.port))
        .await?;
    
    println!("A2A server listening on {}:{}", config.host, config.port);
    
    axum::serve(listener, app).await?;
    
    Ok(())
}