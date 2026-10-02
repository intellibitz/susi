//! A2A executor for susi-gawd
//!
//! Implements the A2A protocol executor for message processing and task management.

use super::capabilities::GawdCapabilities;
use ra2a::error::Result;
use ra2a::server::{AgentExecutor, Event, EventQueue, RequestContext};
use ra2a::types::{Message, Part, Task, TaskState, TaskStatus};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use susi_core::untrusted_content::{enforce_action, wrap_tool_output, ActionClass};

/// Concurrent fleet executions are bounded: each task owns a dedicated OS
/// thread while the fleet runs (see `run_fleet`), and unbounded thread growth
/// is a per-request DoS surface.
const MAX_INFLIGHT_REQUESTS: usize = 32;
static INFLIGHT_REQUESTS: AtomicUsize = AtomicUsize::new(0);

struct InflightPermit;
impl InflightPermit {
    fn try_acquire() -> Option<Self> {
        INFLIGHT_REQUESTS
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
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

/// Runs one A2A task's text as a SUSI mission and returns the answer, or
/// the failure text. Injected so the protocol mapping is testable without a
/// live GAWD plane.
pub type MissionRunner = Arc<dyn Fn(&str) -> std::result::Result<String, String> + Send + Sync>;

/// The production runner: a real mission through the GAWD plane
/// (`plane_bus::gawd::solve_mission`, served by the swarm master). An empty
/// reply means the plane was unreachable or produced nothing — reported as
/// a failure, never as a completed task.
fn plane_bus_mission(intent: &str) -> std::result::Result<String, String> {
    let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let answer = susi_core::plane_bus::gawd::solve_mission(
        intent,
        &workspace,
        susi_gawd_agents::AlphaSelf::VERSION,
    );
    if answer.trim().is_empty() {
        Err("mission produced no result (GAWD plane unreachable or empty answer)".to_string())
    } else {
        Ok(answer)
    }
}

/// Run the mission on a dedicated OS thread: mission dispatch blocks (IPC,
/// provider calls with their own runtimes), which must not happen on the
/// axum worker driving this executor. The result crosses back on a tokio
/// oneshot so the caller `await`s without blocking the worker.
async fn run_mission(
    runner: MissionRunner,
    content: String,
) -> std::result::Result<String, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(runner(&content));
    });
    rx.await
        .unwrap_or_else(|_| Err("mission thread dropped".to_string()))
}

/// susi-gawd's A2A protocol executor
pub struct GawdA2AExecutor {
    runner: MissionRunner,
    capabilities: GawdCapabilities,
}

impl Default for GawdA2AExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl GawdA2AExecutor {
    /// Executor that answers A2A tasks with real GAWD missions.
    pub fn new() -> Self {
        Self::with_runner(Arc::new(plane_bus_mission))
    }

    /// Executor with an injected mission runner (tests, embedding hosts).
    pub fn with_runner(runner: MissionRunner) -> Self {
        Self {
            runner,
            capabilities: GawdCapabilities::orchestrator(),
        }
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
                protocol_version: ra2a::PROTOCOL_VERSION.to_string(),
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

            // A task without a text part is invalid input at the protocol
            // edge: fail it here instead of relying on the mission layer.
            if content.trim().is_empty() {
                let mut task = Task::new(&ctx.task_id, &ctx.context_id);
                task.status = TaskStatus::with_message(
                    TaskState::Failed,
                    Message::agent(vec![Part::text(
                        "A2A message carried no text part to run as a mission".to_string(),
                    )]),
                );
                queue.send(Event::Task(task))?;
                return Ok(());
            }

            // An authenticated A2A transport does not make the message body
            // trusted. Gate it before the mission can dispatch consequential
            // tools, and never echo a rejected body into the task result.
            let content = match enforce_action(
                &wrap_tool_output("a2a:request", &content),
                ActionClass::Consequential,
            ) {
                Ok(content) => content,
                Err(reason) => {
                    let mut task = Task::new(&ctx.task_id, &ctx.context_id);
                    task.status = TaskStatus::with_message(
                        TaskState::Failed,
                        Message::agent(vec![Part::text(reason)]),
                    );
                    queue.send(Event::Task(task))?;
                    return Ok(());
                }
            };

            // The mission must not run on this executor's tokio worker —
            // see `run_mission`.
            let permit = InflightPermit::try_acquire();
            let (state, response_content) = match permit {
                Some(_) => match run_mission(Arc::clone(&self.runner), content).await {
                    Ok(response) => (
                        TaskState::Completed,
                        wrap_tool_output("a2a:mission-reply", &response).redacted_for_sink(),
                    ),
                    Err(error) => (
                        TaskState::Failed,
                        wrap_tool_output("a2a:mission-error", &error).redacted_for_sink(),
                    ),
                },
                None => (
                    TaskState::Rejected,
                    "Request rejected: fleet at capacity".to_string(),
                ),
            };

            // Create task with response
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status =
                TaskStatus::with_message(state, Message::agent(vec![Part::text(response_content)]));

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
