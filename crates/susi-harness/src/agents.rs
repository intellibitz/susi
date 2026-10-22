//! The agent graph: roles, handoffs and a reviewer gate.
//!
//! One task is one agent turn in the scheduler. A mission that needs more than
//! that — a planner, several coders, a reviewer — is a graph of
//! [`AgentSpec`]s whose runs are recorded on a [`Bus`]. The topology is fixed
//! and small on purpose: plan, fan out the coders, review, and escalate once
//! when the reviewer rejects. Everything an agent may do is in its spec —
//! model alias, tools, budget, isolation — so an agent has no ambient
//! authority.

use crate::money::MicroUsd;
use crate::providers::Registry as Providers;
use crate::tools::Registry as Tools;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

/// Where an agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// Its own worktree and claim.
    Worktree,
    /// The caller's workspace, for read-only roles like a reviewer.
    Shared,
}

/// One role in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    /// Stable role name (`planner`, `coder-a`, `reviewer`).
    pub role: String,
    /// Model alias or id, resolved through the provider registry.
    pub model: String,
    /// Tools this agent may call.
    pub tools: Vec<String>,
    /// Most this agent may spend.
    pub budget: Option<MicroUsd>,
    /// Turn cap for its loop.
    pub max_turns: u32,
    /// Where it runs.
    pub isolation: Isolation,
}

impl AgentSpec {
    /// A worktree agent with the default turn cap.
    #[must_use]
    pub fn new(role: &str, model: &str, tools: &[&str]) -> Self {
        Self {
            role: role.to_string(),
            model: model.to_string(),
            tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
            budget: None,
            max_turns: 25,
            isolation: Isolation::Worktree,
        }
    }

    /// Run in the caller's workspace instead of a worktree.
    #[must_use]
    pub fn shared(mut self) -> Self {
        self.isolation = Isolation::Shared;
        self
    }

    /// Cap what this agent may spend.
    #[must_use]
    pub fn within(mut self, budget: MicroUsd) -> Self {
        self.budget = Some(budget);
        self
    }
}

/// What a message on the bus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// The planner's plan.
    Plan,
    /// A coder's finished work.
    Work,
    /// A reviewer's verdict.
    Review,
    /// The reviewer accepted.
    Approval,
    /// The reviewer rejected and asked for changes.
    Rejection,
}

impl MessageKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Work => "work",
            Self::Review => "review",
            Self::Approval => "approval",
            Self::Rejection => "rejection",
        }
    }
}

/// One handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Sending role.
    pub from: String,
    /// Receiving role (`coders`, `reviewer`, …).
    pub to: String,
    /// What kind of handoff this is.
    pub kind: MessageKind,
    /// Human- and machine-readable detail.
    pub payload: Value,
}

impl Message {
    fn to_json(&self) -> Value {
        json!({
            "from": self.from,
            "to": self.to,
            "kind": self.kind.as_str(),
            "payload": self.payload,
        })
    }
}

/// The append-only record of a graph run.
#[derive(Debug, Default, Clone)]
pub struct Bus {
    messages: Vec<Message>,
}

impl Bus {
    /// Append one message.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Every message, in order.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Append the bus to a JSON-lines file.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the file cannot be opened or written.
    pub fn persist(&self, path: &Path) -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for message in &self.messages {
            writeln!(file, "{}", message.to_json())?;
        }
        Ok(())
    }
}

/// What one agent run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    /// Summary handed to the next role.
    pub summary: String,
    /// Whether the reviewer accepted it (ignored for non-reviewer roles).
    pub accepted: bool,
    /// What the run cost.
    pub cost: MicroUsd,
}

/// Runs one role.
pub trait AgentRunner {
    /// Run `spec` against `goal`.
    ///
    /// # Errors
    ///
    /// Returns a failure message when the run could not be completed.
    fn run(&mut self, spec: &AgentSpec, goal: &str) -> Result<AgentOutcome, String>;
}

/// What a graph run produced.
#[derive(Debug, Clone)]
pub struct Report {
    /// Each role and its outcome, in run order.
    pub outcomes: Vec<(String, AgentOutcome)>,
    /// Everything the run cost.
    pub total: MicroUsd,
    /// The handoffs, in order.
    pub bus: Bus,
}

/// One step of a validated graph, before anything runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStep {
    /// Role that runs.
    pub role: String,
    /// Model alias or id the role uses.
    pub model: String,
    /// Where it runs.
    pub isolation: Isolation,
}

impl AgentStep {
    fn from_spec(spec: &AgentSpec) -> Self {
        Self {
            role: spec.role.clone(),
            model: spec.model.clone(),
            isolation: spec.isolation,
        }
    }
}

/// A fixed topology of agents.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    agents: Vec<AgentSpec>,
}

impl Graph {
    /// A graph over the given agents.
    #[must_use]
    pub fn new(agents: Vec<AgentSpec>) -> Self {
        Self { agents }
    }

    /// The agents in the graph.
    #[must_use]
    pub fn agents(&self) -> &[AgentSpec] {
        &self.agents
    }

    /// Check the graph before it spends anything: unique roles, a planner, a
    /// coder and a reviewer, resolvable models and registered tools.
    ///
    /// # Errors
    ///
    /// Returns the first structural problem found.
    pub fn validate(&self) -> Result<(), String> {
        if self.agents.is_empty() {
            return Err("agent graph is empty".to_string());
        }
        let mut roles = BTreeSet::new();
        for agent in &self.agents {
            if !roles.insert(agent.role.as_str()) {
                return Err(format!("duplicate agent role `{}`", agent.role));
            }
            if agent.max_turns == 0 {
                return Err(format!("agent `{}` has no turns", agent.role));
            }
            if Providers::builtin().resolve(&agent.model).is_none() {
                return Err(format!(
                    "agent `{}` names unknown model `{}`",
                    agent.role, agent.model
                ));
            }
            for tool in &agent.tools {
                if Tools::builtin().get(tool).is_none() {
                    return Err(format!(
                        "agent `{}` names unknown tool `{}`",
                        agent.role, tool
                    ));
                }
            }
        }
        if !roles.contains("planner") {
            return Err("agent graph needs a `planner` role".to_string());
        }
        if !roles.contains("reviewer") {
            return Err("agent graph needs a `reviewer` role".to_string());
        }
        if !self
            .agents
            .iter()
            .any(|agent| agent.role.starts_with("coder"))
        {
            return Err("agent graph needs at least one `coder*` role".to_string());
        }
        Ok(())
    }

    /// The run order after validation: planner, every coder, then the reviewer.
    ///
    /// # Errors
    ///
    /// Returns a validation error.
    pub fn steps(&self) -> Result<Vec<AgentStep>, String> {
        self.validate()?;
        let mut steps = Vec::new();
        if let Some(planner) = self.agents.iter().find(|agent| agent.role == "planner") {
            steps.push(AgentStep::from_spec(planner));
        }
        for coder in self
            .agents
            .iter()
            .filter(|agent| agent.role.starts_with("coder"))
        {
            steps.push(AgentStep::from_spec(coder));
        }
        if let Some(reviewer) = self.agents.iter().find(|agent| agent.role == "reviewer") {
            steps.push(AgentStep::from_spec(reviewer));
        }
        Ok(steps)
    }

    /// Plan, fan out the coders, review, and escalate once on rejection.
    ///
    /// # Errors
    ///
    /// Returns a validation error, a runner error, an over-budget agent, or a
    /// second rejection.
    pub fn run<R: AgentRunner>(&self, runner: &mut R, goal: &str) -> Result<Report, String> {
        self.validate()?;
        let mut bus = Bus::default();
        let mut outcomes = Vec::new();
        let mut total = MicroUsd::ZERO;

        let planner = self
            .agents
            .iter()
            .find(|agent| agent.role == "planner")
            .ok_or_else(|| "agent graph needs a `planner` role".to_string())?;
        let plan = runner.run(planner, goal)?;
        charge(planner, &plan, &mut total)?;
        bus.push(Message {
            from: planner.role.clone(),
            to: "coders".to_string(),
            kind: MessageKind::Plan,
            payload: json!({"summary": plan.summary}),
        });
        outcomes.push((planner.role.clone(), plan));

        let coders = self
            .agents
            .iter()
            .filter(|agent| agent.role.starts_with("coder"))
            .collect::<Vec<_>>();
        if coders.is_empty() {
            return Err("agent graph needs at least one `coder*` role".to_string());
        }
        let mut work = Vec::new();
        for coder in &coders {
            let outcome = runner.run(coder, goal)?;
            charge(coder, &outcome, &mut total)?;
            bus.push(Message {
                from: coder.role.clone(),
                to: "reviewer".to_string(),
                kind: MessageKind::Work,
                payload: json!({"summary": outcome.summary}),
            });
            work.push(outcome.summary.clone());
            outcomes.push((coder.role.clone(), outcome));
        }

        let reviewer = self
            .agents
            .iter()
            .find(|agent| agent.role == "reviewer")
            .ok_or_else(|| "agent graph needs a `reviewer` role".to_string())?;
        let mut verdict = runner.run(reviewer, &work.join("\n"))?;
        charge(reviewer, &verdict, &mut total)?;
        bus.push(review_message(reviewer, &verdict));

        if !verdict.accepted {
            let first = coders
                .first()
                .ok_or_else(|| "agent graph needs at least one `coder*` role".to_string())?;
            let retry_goal = format!(
                "{goal}\n\nThe reviewer rejected the previous attempt: {}",
                verdict.summary
            );
            bus.push(Message {
                from: "supervisor".to_string(),
                to: first.role.clone(),
                kind: MessageKind::Rejection,
                payload: json!({"reason": verdict.summary}),
            });
            let retry = runner.run(first, &retry_goal)?;
            charge(first, &retry, &mut total)?;
            let retry_summary = retry.summary.clone();
            bus.push(Message {
                from: first.role.clone(),
                to: "reviewer".to_string(),
                kind: MessageKind::Work,
                payload: json!({"summary": retry_summary, "retry": true}),
            });
            outcomes.push((first.role.clone(), retry));

            verdict = runner.run(reviewer, &retry_summary)?;
            charge(reviewer, &verdict, &mut total)?;
            bus.push(review_message(reviewer, &verdict));
            if !verdict.accepted {
                return Err(format!(
                    "reviewer rejected `{}` twice: {}",
                    first.role, verdict.summary
                ));
            }
        }

        outcomes.push((reviewer.role.clone(), verdict));
        Ok(Report {
            outcomes,
            total,
            bus,
        })
    }
}

fn review_message(reviewer: &AgentSpec, verdict: &AgentOutcome) -> Message {
    Message {
        from: reviewer.role.clone(),
        to: "coders".to_string(),
        kind: if verdict.accepted {
            MessageKind::Approval
        } else {
            MessageKind::Rejection
        },
        payload: json!({"summary": verdict.summary}),
    }
}

fn charge(spec: &AgentSpec, outcome: &AgentOutcome, total: &mut MicroUsd) -> Result<(), String> {
    if let Some(limit) = spec.budget {
        if outcome.cost.micros() > limit.micros() {
            return Err(format!(
                "agent `{}` spent {} over its budget {}",
                spec.role, outcome.cost, limit
            ));
        }
    }
    *total = total.saturating_add(outcome.cost);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Fake {
        calls: Vec<String>,
        verdicts: VecDeque<bool>,
        cost: MicroUsd,
    }

    impl Fake {
        fn new(verdicts: &[bool]) -> Self {
            Self {
                calls: Vec::new(),
                verdicts: verdicts.iter().copied().collect(),
                cost: MicroUsd::ZERO,
            }
        }
    }

    impl AgentRunner for Fake {
        fn run(&mut self, spec: &AgentSpec, _goal: &str) -> Result<AgentOutcome, String> {
            self.calls.push(spec.role.clone());
            let accepted = if spec.role == "reviewer" {
                self.verdicts.pop_front().unwrap_or(true)
            } else {
                true
            };
            Ok(AgentOutcome {
                summary: format!("{} result", spec.role),
                accepted,
                cost: self.cost,
            })
        }
    }

    fn graph() -> Graph {
        Graph::new(vec![
            AgentSpec::new("planner", "planner", &["view_file"]),
            AgentSpec::new("coder-a", "coder", &["view_file", "write_to_file"]),
            AgentSpec::new("coder-b", "coder", &["view_file", "write_to_file"]),
            AgentSpec::new("reviewer", "planner", &["view_file"]).shared(),
        ])
    }

    #[test]
    fn agent_steps_follow_the_topology() {
        let steps = graph().steps().unwrap();
        let roles: Vec<&str> = steps.iter().map(|step| step.role.as_str()).collect();
        assert_eq!(roles, vec!["planner", "coder-a", "coder-b", "reviewer"]);
        assert_eq!(steps[0].model, "planner");
        assert_eq!(steps[1].model, "coder");
        assert_eq!(steps[3].isolation, Isolation::Shared);
    }

    #[test]
    fn agent_graph_runs_plan_fanout_and_review() {
        let mut fake = Fake::new(&[true]);
        let report = graph().run(&mut fake, "ship it").unwrap();
        assert_eq!(
            fake.calls,
            vec!["planner", "coder-a", "coder-b", "reviewer"]
        );
        assert_eq!(report.outcomes.len(), 4);
        assert_eq!(report.total, MicroUsd::ZERO);
        let kinds = report
            .bus
            .messages()
            .iter()
            .map(|message| message.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                MessageKind::Plan,
                MessageKind::Work,
                MessageKind::Work,
                MessageKind::Approval
            ]
        );
    }

    #[test]
    fn agent_graph_reviewer_rejects_once_then_approves() {
        let mut fake = Fake::new(&[false, true]);
        let report = graph().run(&mut fake, "ship it").unwrap();
        assert_eq!(
            fake.calls,
            vec!["planner", "coder-a", "coder-b", "reviewer", "coder-a", "reviewer"]
        );
        assert!(report
            .bus
            .messages()
            .iter()
            .any(|message| message.kind == MessageKind::Rejection));
    }

    #[test]
    fn agent_graph_stops_on_a_second_rejection() {
        let mut fake = Fake::new(&[false, false]);
        let error = graph().run(&mut fake, "ship it").unwrap_err();
        assert!(error.contains("rejected"), "{error}");
    }

    #[test]
    fn agent_graph_requires_planner_coder_and_reviewer() {
        let no_reviewer = Graph::new(vec![AgentSpec::new("planner", "planner", &[])]);
        assert!(no_reviewer.validate().unwrap_err().contains("reviewer"));
        let no_coder = Graph::new(vec![
            AgentSpec::new("planner", "planner", &[]),
            AgentSpec::new("reviewer", "planner", &[]),
        ]);
        assert!(no_coder.validate().unwrap_err().contains("coder"));
    }

    #[test]
    fn agent_graph_validates_models_and_tools() {
        let bad_model = Graph::new(vec![
            AgentSpec::new("planner", "mystery-model", &[]),
            AgentSpec::new("coder-a", "coder", &[]),
            AgentSpec::new("reviewer", "planner", &[]),
        ]);
        assert!(bad_model.validate().unwrap_err().contains("unknown model"));
        let bad_tool = Graph::new(vec![
            AgentSpec::new("planner", "planner", &["nope"]),
            AgentSpec::new("coder-a", "coder", &[]),
            AgentSpec::new("reviewer", "planner", &[]),
        ]);
        assert!(bad_tool.validate().unwrap_err().contains("unknown tool"));
    }

    #[test]
    fn agent_graph_refuses_an_agent_over_budget() {
        let mut fake = Fake::new(&[true]);
        fake.cost = MicroUsd::from_micros(2);
        let graph = Graph::new(vec![
            AgentSpec::new("planner", "planner", &[]),
            AgentSpec::new("coder-a", "coder", &[]).within(MicroUsd::from_micros(1)),
            AgentSpec::new("reviewer", "planner", &[]),
        ]);
        let error = graph.run(&mut fake, "ship it").unwrap_err();
        assert!(error.contains("over its budget"), "{error}");
    }

    #[test]
    fn agent_graph_bus_persists_as_json_lines() {
        let mut fake = Fake::new(&[true]);
        let report = graph().run(&mut fake, "ship it").unwrap();
        let path = std::env::temp_dir().join(format!("susi-bus-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        report.bus.persist(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), report.bus.messages().len());
        assert!(text.contains("\"kind\":\"plan\""));
        let _ = std::fs::remove_file(&path);
    }
}
