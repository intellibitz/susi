//! A2A executor for susi-gawd
//!
//! Implements the A2A protocol executor for message processing and task management.

use super::capabilities::GawdCapabilities;
use crate::protocol::{
    AgentCard, AgentExecutor, AgentInterface, AgentSkill, EventQueue, HttpAuthSecurityScheme,
    Message, Part, PartContent, RequestContext, Result, SecurityRequirement, SecurityScheme, Task,
    TaskState, TaskStatus,
};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use susi_core::acp::{AcpContentBlock, AcpMessageRole, AcpTurnContext, AcpTurnExecutor};
use susi_core::untrusted_content::{enforce_action, wrap_tool_output, ActionClass};

/// Time-windowed SLO sample measured from A2A execution, queue, inference and recovery events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SloSample {
    /// Window length in seconds over which events were aggregated.
    pub window_secs: u64,
    pub availability_num: u64,
    pub availability_den: u64,
    pub task_success_num: u64,
    pub task_success_den: u64,
    pub queue_delay_total_us: u64,
    pub queue_delay_samples: u64,
    pub inference_latency_total_us: u64,
    pub inference_latency_samples: u64,
    pub recovery_duration_total_us: u64,
    pub recovery_duration_samples: u64,
}

impl SloSample {
    /// Formats all 5 SLO metrics with explicit windows and `"unknown"` markers below `min_samples`.
    #[must_use]
    pub fn publish_slos(&self, min_samples: u64) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        out.insert("window_secs".to_string(), format!("{}s", self.window_secs));

        let fmt_ratio = |num: u64, den: u64| -> String {
            if den < min_samples || den == 0 {
                format!("unknown (0/{den}, min_samples={min_samples})")
            } else {
                let pct = (num as f64 / den as f64) * 100.0;
                format!("{pct:.2}% ({num}/{den}, window={}s)", self.window_secs)
            }
        };

        let fmt_latency = |total_us: u64, samples: u64| -> String {
            if samples < min_samples || samples == 0 {
                format!("unknown (0/{samples}, min_samples={min_samples})")
            } else {
                let mean_us = total_us / samples;
                format!(
                    "{mean_us}us ({total_us}us/{samples}, window={}s)",
                    self.window_secs
                )
            }
        };

        out.insert(
            "availability".to_string(),
            fmt_ratio(self.availability_num, self.availability_den),
        );
        out.insert(
            "task_success".to_string(),
            fmt_ratio(self.task_success_num, self.task_success_den),
        );
        out.insert(
            "queue_delay".to_string(),
            fmt_latency(self.queue_delay_total_us, self.queue_delay_samples),
        );
        out.insert(
            "inference_latency".to_string(),
            fmt_latency(
                self.inference_latency_total_us,
                self.inference_latency_samples,
            ),
        );
        out.insert(
            "recovery_duration".to_string(),
            fmt_latency(
                self.recovery_duration_total_us,
                self.recovery_duration_samples,
            ),
        );
        out
    }
}

#[derive(Debug, Clone)]
struct RecordedTaskEvent {
    recorded_unix_secs: u64,
    state: TaskState,
    queue_delay_us: Option<u64>,
    inference_latency_us: Option<u64>,
    recovery_duration_us: Option<u64>,
}

#[derive(Debug, Default)]
struct SloEventCollector {
    events: Vec<RecordedTaskEvent>,
    pending_recovery_starts: BTreeMap<String, Instant>,
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bound concurrent fleet executions because each task owns a dedicated OS thread.
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

/// Production A2A runner through the GAWD plane; empty replies are failures.
fn plane_bus_mission(intent: &str) -> std::result::Result<String, String> {
    let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    plane_bus_mission_at(intent, workspace)
}

fn plane_bus_mission_at(
    intent: &str,
    workspace: std::path::PathBuf,
) -> std::result::Result<String, String> {
    susi_core::plane_bus::gawd::solve_mission_for_acp(
        intent,
        &workspace,
        susi_gawd_agents::AlphaSelf::VERSION,
    )
}

fn plane_bus_acp_mission(
    intent: &str,
    workspace: Option<String>,
) -> std::result::Result<String, String> {
    let workspace = workspace.map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    plane_bus_mission_at(intent, workspace)
}

/// Run blocking mission dispatch on a dedicated OS thread and await its result.
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

/// ACP turns answered by the same guarded GAWD mission as A2A tasks.
pub struct GawdAcpExecutor {
    runner: AcpMissionRunner,
}

pub type AcpMissionRunner =
    Arc<dyn Fn(&str, Option<String>) -> std::result::Result<String, String> + Send + Sync>;

impl Default for GawdAcpExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl GawdAcpExecutor {
    /// Executor that answers ACP turns with real GAWD missions.
    pub fn new() -> Self {
        Self::with_workspace_runner(Arc::new(plane_bus_acp_mission))
    }

    /// Executor with an injected mission runner (tests, embedding hosts).
    pub fn with_runner(runner: MissionRunner) -> Self {
        Self::with_workspace_runner(Arc::new(move |intent, _workspace| runner(intent)))
    }

    pub(crate) fn with_workspace_runner(runner: AcpMissionRunner) -> Self {
        Self { runner }
    }
}

const HISTORY_BUDGET: usize = 16 * 1024;

/// Prefix the gated prompt with the newest safe turns from the session history.
fn mission_text(ctx: &AcpTurnContext, prompt: String) -> String {
    let mut earlier = String::new();
    let messages = ctx.inference_messages();
    for m in &messages[..messages.len() - 1] {
        let line = match m.role {
            AcpMessageRole::User if gated(&m.content).is_ok() => format!("Asked: {}\n", m.content),
            AcpMessageRole::Assistant => format!("Answered: {}\n", m.content),
            AcpMessageRole::System | AcpMessageRole::User | AcpMessageRole::Tool => continue,
        };
        earlier.push_str(&line);
    }
    let mut cut = earlier.len().saturating_sub(HISTORY_BUDGET);
    while !earlier.is_char_boundary(cut) {
        cut += 1;
    }
    match &earlier[cut..] {
        "" => prompt,
        kept => format!("Earlier in this session:\n{kept}\nNow:\n{prompt}"),
    }
}

/// Gate ACP text before dispatching consequential tools.
fn gated(text: &str) -> std::result::Result<String, String> {
    enforce_action(
        &wrap_tool_output("acp:request", text),
        ActionClass::Consequential,
    )
}

impl AcpTurnExecutor for GawdAcpExecutor {
    /// Blocks for the mission; cancellation drops an in-flight answer.
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> std::result::Result<Vec<AcpContentBlock>, String> {
        let content = ctx.joined_prompt_text();
        if content.trim().is_empty() {
            return Err("ACP prompt carried no text to run as a mission".to_string());
        }
        let content = mission_text(ctx, gated(&content)?);
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let _permit = InflightPermit::try_acquire()
            .ok_or_else(|| "Request rejected: fleet at capacity".to_string())?;
        let answer = (self.runner)(&content, ctx.workspace.clone())
            .map_err(|error| wrap_tool_output("acp:mission-error", &error).redacted_for_sink())?;
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let answer = wrap_tool_output("acp:mission-reply", &answer).redacted_for_sink();
        emit_delta(&answer);
        Ok(vec![AcpContentBlock::text(answer)])
    }
}

/// susi-gawd's A2A protocol executor
pub struct GawdA2AExecutor {
    runner: MissionRunner,
    capabilities: GawdCapabilities,
    slo_collector: Arc<Mutex<SloEventCollector>>,
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
            slo_collector: Arc::new(Mutex::new(SloEventCollector::default())),
        }
    }

    /// Aggregates recorded task events within `window_secs` into a measured [`SloSample`].
    #[must_use]
    pub fn slo_sample(&self, window_secs: u64) -> SloSample {
        let now = now_unix_secs();
        let cutoff = now.saturating_sub(window_secs);
        let mut sample = SloSample {
            window_secs,
            availability_num: 0,
            availability_den: 0,
            task_success_num: 0,
            task_success_den: 0,
            queue_delay_total_us: 0,
            queue_delay_samples: 0,
            inference_latency_total_us: 0,
            inference_latency_samples: 0,
            recovery_duration_total_us: 0,
            recovery_duration_samples: 0,
        };
        if let Ok(guard) = self.slo_collector.lock() {
            for ev in guard
                .events
                .iter()
                .filter(|e| e.recorded_unix_secs >= cutoff)
            {
                match ev.state {
                    TaskState::Completed => {
                        sample.availability_num += 1;
                        sample.availability_den += 1;
                        sample.task_success_num += 1;
                        sample.task_success_den += 1;
                    }
                    TaskState::Failed => {
                        sample.availability_num += 1;
                        sample.availability_den += 1;
                        sample.task_success_den += 1;
                    }
                    TaskState::Rejected => {
                        sample.availability_den += 1;
                        sample.task_success_den += 1;
                    }
                    TaskState::Canceled => {
                        sample.task_success_den += 1;
                    }
                    TaskState::Unspecified
                    | TaskState::Submitted
                    | TaskState::Working
                    | TaskState::InputRequired
                    | TaskState::AuthRequired => {}
                }
                if let Some(q_us) = ev.queue_delay_us {
                    sample.queue_delay_total_us = sample.queue_delay_total_us.saturating_add(q_us);
                    sample.queue_delay_samples += 1;
                }
                if let Some(inf_us) = ev.inference_latency_us {
                    sample.inference_latency_total_us =
                        sample.inference_latency_total_us.saturating_add(inf_us);
                    sample.inference_latency_samples += 1;
                }
                if let Some(rec_us) = ev.recovery_duration_us {
                    sample.recovery_duration_total_us =
                        sample.recovery_duration_total_us.saturating_add(rec_us);
                    sample.recovery_duration_samples += 1;
                }
            }
        }
        sample
    }

    /// Publishes time-windowed operating-layer SLOs formatted with explicit denominators
    /// and `"unknown"` markers when samples are below `min_samples`.
    #[must_use]
    pub fn publish_slos(&self, window_secs: u64, min_samples: u64) -> BTreeMap<String, String> {
        self.slo_sample(window_secs).publish_slos(min_samples)
    }

    pub fn agent_card(&self) -> AgentCard {
        AgentCard {
            name: "susi-gawd".to_string(),
            description: "SUSI GAWD: AI agent orchestrator and swarm supervisor".to_string(),
            // The workspace package version, stamped at build time from the
            // root Cargo.toml (`GEN_ENGINE_VERSION`) — the same version
            // `susi --version` reports, never a hand-maintained literal.
            version: susi_gawd_agents::AlphaSelf::VERSION.to_string(),
            // Only JSONRPC is advertised: the HTTP+JSON (REST) binding is
            // not served (see `server::serve`). The card must not claim it.
            supported_interfaces: vec![AgentInterface {
                url: "/".to_string(),
                protocol_binding: crate::protocol::JSONRPC_BINDING.to_string(),
                tenant: None,
                protocol_version: crate::protocol::PROTOCOL_VERSION.to_string(),
            }],
            documentation_url: Some("https://github.com/intellibitz/susi".to_string()),
            capabilities: self.capabilities.as_capabilities(),
            skills: Self::skills(),
            security_schemes: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "bearer".to_string(),
                    SecurityScheme::Http(HttpAuthSecurityScheme {
                        scheme: "Bearer".to_string(),
                    }),
                );
                m
            },
            security_requirements: vec![SecurityRequirement {
                schemes: {
                    let mut m = std::collections::HashMap::new();
                    m.insert("bearer".to_string(), Vec::new());
                    m
                },
            }],
            default_input_modes: vec!["text/plain".to_string()],
            default_output_modes: vec!["text/plain".to_string()],
        }
    }
}

/// One advertised skill. Every skill is served by the same executor: a
/// text part in, run as a GAWD mission, the evidence-gated answer out
/// (`message/send`, or `message/stream` on `/stream`).
fn skill(id: &str, description: &str, tags: &[&str], example: Option<&str>) -> AgentSkill {
    AgentSkill {
        id: id.to_string(),
        name: id.to_string(),
        description: description.to_string(),
        tags: tags.iter().map(|t| (*t).to_string()).collect(),
        examples: example.into_iter().map(str::to_string).collect(),
        input_modes: vec!["text/plain".to_string()],
        output_modes: vec!["text/plain".to_string()],
    }
}

impl GawdA2AExecutor {
    /// Skills on the card: the per-capability entries (missions, agents,
    /// frameworks, models, harness, extensions) plus the two original
    /// orchestration skills, kept so existing clients that match on their ids
    /// still find them. The descriptions say what actually happens: every
    /// request is a mission, and the capability names what the mission can reach.
    #[must_use]
    pub fn skills() -> Vec<AgentSkill> {
        vec![
            skill(
                "missions",
                "Run a natural-language goal as a GAWD mission: planned, dispatched to tools and agents, and answered only with evidence-gated results.",
                &["missions", "orchestration"],
                Some("Summarize the open tasks in this workspace"),
            ),
            skill(
                "agents",
                "Missions that hand work to the external task executors installed on this host (the agents managed by `susi agents`; MCP tools `agents_list`, `agents_run`).",
                &["agents", "delegation"],
                Some("Use an installed coding agent to fix the failing test"),
            ),
            skill(
                "frameworks",
                "Missions that run through the agent frameworks/engines installed on this host, such as LangGraph or CrewAI (managed by `susi frameworks`; MCP tools `frameworks_list`, `frameworks_run`).",
                &["frameworks", "engines"],
                None,
            ),
            skill(
                "models",
                "Missions answered through GEMI model routing: local open-weight models and configured cloud providers, chosen by placement policy.",
                &["models", "inference"],
                None,
            ),
            skill(
                "harness",
                "Missions and autonomous budget/pareto optimization governed by the SUSI Harness and Bayesian UCB1 / SCIS / SOVA routing (managed by `susi harness` and `susi brain`; MCP tools `harness_*`, `brain_pareto`).",
                &["harness", "brain", "routing"],
                None,
            ),
            skill(
                "extensions",
                "Missions that validate, inspect, or admit signed/checksum-bound extension packs and Wasm reflexes (managed by `susi extensions`; MCP tools `extensions_*`).",
                &["extensions", "wasm", "reflex"],
                None,
            ),
            skill(
                "agent_orchestration",
                "Orchestrate multi-agent missions and task delegation",
                &["orchestration", "swarm"],
                None,
            ),
            skill(
                "task_delegation",
                "Delegate tasks to specialized agents",
                &["delegation", "coordination"],
                None,
            ),
        ]
    }
}

impl AgentExecutor for GawdA2AExecutor {
    fn execute<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let enqueue_instant = Instant::now();
            // Extract message content
            let content = ctx
                .message
                .as_ref()
                .and_then(|m| m.parts.first())
                .and_then(|part| {
                    if let PartContent::Text(text) = &part.content {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            // A task without a text part is invalid input at the protocol
            // edge: fail it here instead of relying on the mission layer.
            if content.trim().is_empty() {
                if let Ok(mut guard) = self.slo_collector.lock() {
                    guard
                        .pending_recovery_starts
                        .insert(ctx.task_id.clone(), Instant::now());
                    guard.events.push(RecordedTaskEvent {
                        recorded_unix_secs: now_unix_secs(),
                        state: TaskState::Failed,
                        queue_delay_us: Some(enqueue_instant.elapsed().as_micros() as u64),
                        inference_latency_us: None,
                        recovery_duration_us: None,
                    });
                }
                let mut task = Task::new(&ctx.task_id, &ctx.context_id);
                task.status = TaskStatus::with_message(
                    TaskState::Failed,
                    Message::agent(vec![Part::text(
                        "A2A message carried no text part to run as a mission".to_string(),
                    )]),
                );
                queue.send(task)?;
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
                    if let Ok(mut guard) = self.slo_collector.lock() {
                        guard
                            .pending_recovery_starts
                            .insert(ctx.task_id.clone(), Instant::now());
                        guard.events.push(RecordedTaskEvent {
                            recorded_unix_secs: now_unix_secs(),
                            state: TaskState::Failed,
                            queue_delay_us: Some(enqueue_instant.elapsed().as_micros() as u64),
                            inference_latency_us: None,
                            recovery_duration_us: None,
                        });
                    }
                    let mut task = Task::new(&ctx.task_id, &ctx.context_id);
                    task.status = TaskStatus::with_message(
                        TaskState::Failed,
                        Message::agent(vec![Part::text(reason)]),
                    );
                    queue.send(task)?;
                    return Ok(());
                }
            };

            // The mission must not run on this executor's tokio worker —
            // see `run_mission`.
            let permit = InflightPermit::try_acquire();
            let exec_start_instant = Instant::now();
            let queue_delay_us = exec_start_instant
                .saturating_duration_since(enqueue_instant)
                .as_micros() as u64;
            let (state, response_content, inference_latency_us) = match permit {
                Some(_) => {
                    let res = run_mission(Arc::clone(&self.runner), content).await;
                    let inf_us = exec_start_instant.elapsed().as_micros() as u64;
                    match res {
                        Ok(response) => (
                            TaskState::Completed,
                            wrap_tool_output("a2a:mission-reply", &response).redacted_for_sink(),
                            Some(inf_us),
                        ),
                        Err(error) => (
                            TaskState::Failed,
                            wrap_tool_output("a2a:mission-error", &error).redacted_for_sink(),
                            Some(inf_us),
                        ),
                    }
                }
                None => (
                    TaskState::Rejected,
                    "Request rejected: fleet at capacity".to_string(),
                    None,
                ),
            };

            if let Ok(mut guard) = self.slo_collector.lock() {
                let recovery_duration_us = if state == TaskState::Completed {
                    guard
                        .pending_recovery_starts
                        .remove(&ctx.task_id)
                        .map(|started| started.elapsed().as_micros() as u64)
                } else {
                    guard
                        .pending_recovery_starts
                        .entry(ctx.task_id.clone())
                        .or_insert_with(Instant::now);
                    None
                };
                guard.events.push(RecordedTaskEvent {
                    recorded_unix_secs: now_unix_secs(),
                    state,
                    queue_delay_us: Some(queue_delay_us),
                    inference_latency_us,
                    recovery_duration_us,
                });
            }

            // Create task with response
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status =
                TaskStatus::with_message(state, Message::agent(vec![Part::text(response_content)]));

            queue.send(task)?;
            Ok(())
        })
    }

    fn cancel<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.slo_collector.lock() {
                guard
                    .pending_recovery_starts
                    .insert(ctx.task_id.clone(), Instant::now());
                guard.events.push(RecordedTaskEvent {
                    recorded_unix_secs: now_unix_secs(),
                    state: TaskState::Canceled,
                    queue_delay_us: None,
                    inference_latency_us: None,
                    recovery_duration_us: None,
                });
            }
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status = TaskStatus::new(TaskState::Canceled);
            queue.send(task)?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "tests/executor.rs"]
mod tests;
