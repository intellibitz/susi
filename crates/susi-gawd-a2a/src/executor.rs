//! A2A executor for susi-gawd
//!
//! Implements the A2A protocol executor for message processing and task management.

use super::capabilities::GawdCapabilities;
use super::task_store::GawdTaskStore;
use ra2a::error::Result;
use ra2a::server::{AgentExecutor, Event, EventQueue, RequestContext};
use ra2a::types::{Message, Part, Task, TaskState, TaskStatus};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use susi_gawd_agents::GawdAgentFleet;

/// Concurrent fleet executions are bounded: each task owns a dedicated OS
/// thread while the fleet runs (see `run_fleet`), and unbounded thread growth
/// is a per-request DoS surface.
const MAX_INFLIGHT_REQUESTS: usize = 32;
static INFLIGHT_REQUESTS: AtomicUsize = AtomicUsize::new(0);

struct InflightPermit;
impl InflightPermit {
    fn try_acquire() -> Option<Self> {
        INFLIGHT_REQUESTS
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_INFLIGHT_REQUESTS).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for InflightPermit {
    fn drop(&mut self) {
        INFLIGHT_REQUESTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run `GawdAgentFleet::process_request` on a dedicated OS thread.
///
/// The fleet's provider dispatch owns a `current_thread` tokio runtime and
/// `block_on`s it — legal on a plain thread, but a panic inside the axum
/// worker runtime that drives this executor. The result crosses back on a
/// tokio oneshot so the caller `await`s without blocking the worker.
async fn run_fleet(
    fleet: Arc<GawdAgentFleet>,
    content: String,
) -> std::result::Result<String, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(fleet.process_request(&content)),
            Err(e) => Err(format!("fleet runtime unavailable: {e}")),
        };
        let _ = tx.send(result);
    });
    rx.await
        .unwrap_or_else(|_| Err("fleet thread dropped".to_string()))
}

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
            // Only JSONRPC is advertised: ra2a's axum REST router uses
            // path syntax axum rejects, so the HTTP+JSON binding is not
            // served (see `server::serve`). The card must not claim it.
            supported_interfaces: vec![ra2a::types::AgentInterface {
                url: "/".to_string(),
                protocol_binding: ra2a::types::TransportProtocol::new(
                    ra2a::types::TransportProtocol::JSONRPC,
                ),
                tenant: None,
                protocol_version: "1.0".to_string(),
            }],
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
                    input_modes: vec!["text".to_string()],
                    output_modes: vec!["text".to_string()],
                    security_requirements: vec![],
                },
                ra2a::types::AgentSkill {
                    id: "task_delegation".to_string(),
                    name: "task_delegation".to_string(),
                    description: "Delegate tasks to specialized agents".to_string(),
                    tags: vec!["delegation".to_string(), "coordination".to_string()],
                    examples: vec![],
                    input_modes: vec!["text".to_string()],
                    output_modes: vec!["text".to_string()],
                    security_requirements: vec![],
                },
            ],
            security_schemes: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "bearer".to_string(),
                    ra2a::types::SecurityScheme::Http(
                        ra2a::types::HttpAuthSecurityScheme::bearer(),
                    ),
                );
                m
            },
            security_requirements: vec![ra2a::types::SecurityRequirement {
                schemes: {
                    let mut m = std::collections::HashMap::new();
                    m.insert("bearer".to_string(), Vec::new());
                    m
                },
            }],
            default_input_modes: vec!["text".to_string()],
            default_output_modes: vec!["text".to_string()],
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

            // Route to appropriate agent based on message content. The fleet
            // must not run on this executor's tokio worker — see `run_fleet`.
            let _permit = InflightPermit::try_acquire();
            let response_content = match _permit {
                Some(_) => run_fleet(Arc::clone(&self.agent_fleet), content)
                    .await
                    .unwrap_or_else(|_| "Request processing failed".to_string()),
                None => "Request rejected: fleet at capacity".to_string(),
            };

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
