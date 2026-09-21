//! A2A executor for susi-gawd
//!
//! Implements the A2A protocol executor for message processing and task management.

use super::capabilities::GawdCapabilities;
use super::task_store::GawdTaskStore;
use ra2a::error::Result;
use ra2a::server::{AgentExecutor, Event, EventQueue, RequestContext};
use ra2a::types::{Message, Part, Task, TaskState, TaskStatus};
use std::pin::Pin;
use std::sync::Arc;
use susi_gawd_agents::GawdAgentFleet;

/// susi-gawd's A2A protocol executor
pub struct GawdA2AExecutor {
    agent_fleet: Arc<GawdAgentFleet>,
    task_store: GawdTaskStore,
    capabilities: GawdCapabilities,
}

impl GawdA2AExecutor {
    pub fn new(agent_fleet: Arc<GawdAgentFleet>) -> Self {
        Self {
            agent_fleet,
            task_store: GawdTaskStore::new(),
            capabilities: GawdCapabilities::orchestrator(),
        }
    }

    pub fn task_store(&self) -> GawdTaskStore {
        self.task_store.clone()
    }

    pub fn agent_card(&self) -> ra2a::types::AgentCard {
        ra2a::types::AgentCard {
            name: "susi-gawd".to_string(),
            description: "SUSI GAWD: AI agent orchestrator and swarm supervisor".to_string(),
            version: susi_gawd_agents::AlphaSelf::VERSION.to_string(),
            supported_interfaces: vec![
                ra2a::types::AgentInterface {
                    url: "/rpc".to_string(),
                    protocol_binding: ra2a::types::TransportProtocol::new(
                        ra2a::types::TransportProtocol::JSONRPC,
                    ),
                    tenant: None,
                    protocol_version: "1.0".to_string(),
                },
                ra2a::types::AgentInterface {
                    url: "/".to_string(),
                    protocol_binding: ra2a::types::TransportProtocol::new(
                        ra2a::types::TransportProtocol::HTTP_JSON,
                    ),
                    tenant: None,
                    protocol_version: "1.0".to_string(),
                },
            ],
            provider: None,
            documentation_url: Some("https://github.com/intellibitz/susi".to_string()),
            capabilities: self.capabilities.as_capabilities(),
            skills: vec![
                ra2a::types::AgentSkill {
                    id: "agent_orchestration".to_string(),
                    name: "agent_orchestration".to_string(),
                    description: "Orchestrate multi-agent missions and task delegation".to_string(),
                    tags: vec!["orchestration".to_string(), "swarm".to_string()],
                    examples: vec![],
                    input_modes: vec![],
                    output_modes: vec![],
                    security_requirements: vec![],
                },
                ra2a::types::AgentSkill {
                    id: "task_delegation".to_string(),
                    name: "task_delegation".to_string(),
                    description: "Delegate tasks to specialized agents".to_string(),
                    tags: vec!["delegation".to_string(), "coordination".to_string()],
                    examples: vec![],
                    input_modes: vec![],
                    output_modes: vec![],
                    security_requirements: vec![],
                },
            ],
            security_schemes: Default::default(),
            security_requirements: vec![],
            default_input_modes: vec![],
            default_output_modes: vec![],
            signatures: vec![],
            icon_url: None,
        }
    }
}

impl AgentExecutor for GawdA2AExecutor {
    fn execute<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Extract message content
            let content = ctx
                .message
                .as_ref()
                .and_then(|m| m.parts.first())
                .and_then(|part| {
                    if let ra2a::types::PartContent::Text(text) = &part.content {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            // Route to appropriate agent based on message content
            let response_content = self
                .agent_fleet
                .process_request(&content)
                .await
                .unwrap_or_else(|_| "Request processing failed".to_string());

            // Create task with response
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status = TaskStatus::with_message(
                TaskState::Completed,
                Message::agent(vec![Part::text(response_content)]),
            );

            queue.send(Event::Task(task))?;
            Ok(())
        })
    }

    fn cancel<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status = TaskStatus::new(TaskState::Canceled);
            queue.send(Event::Task(task))?;
            Ok(())
        })
    }
}
