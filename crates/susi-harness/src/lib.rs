#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! SUSI's self-build harness.
//!
//! This crate is the contract between a durable **supervisor** and every
//! client that drives it (the commercial desktop, an agent shelling out to the
//! CLI, an MCP client). Core ships no UI: clients read the JSON/SSE API. The supervisor loop and the
//! executor pool land in later phases; everything here is consumed by them.
//!
//! The split is deliberate:
//!
//! - [`money`] and [`pricing`] make cost arithmetic reproducible — integer
//!   micro-USD, prices per million tokens, cache hits priced far below misses.
//! - [`catalog`] holds the price data itself: a model the catalogue cannot
//!   price still runs at a zero recorded price, and a capped account is
//!   refused it, because its spend could not be counted against the cap.
//! - [`budget`] admits an attempt only when its worst-case estimate, the money
//!   already spent and the attempts already in flight all fit the mission cap.
//! - [`policy`] ranks models by expected cost per accepted task (learned from
//!   the ledger) and escalates on failure without retrying the same model.
//! - [`cockpit`] is the harness API: spend per mission and the event feed as
//!   JSON and SSE, plus the operator command endpoint. It serves no HTML.
//! - [`usage`] turns a provider's own JSON into [`pricing::TokenUsage`],
//!   including DeepSeek's and Gemini's cache-hit counts.
//! - [`ledger`] is the append-only record of every attempt, priced at the rate
//!   in force when it ran, so spend can always be explained after the fact.
//! - [`event`] is the live, human-readable feed: one row per action, no code.
//! - [`gateway`] meters a completion: forward it, price the provider's own
//!   usage, and append the ledger row before answering.
//! - [`executor`] runs one native worker turn to a deadline, with a per-run key
//!   so the gateway attributes every call it makes.
//! - [`scheduler`] runs a mission's tasks concurrently, trying the candidate
//!   models in order and recording every attempt.
//! - [`adapter`] makes a susi target's own rules mechanical: worktree, claim,
//!   acceptance, finish.
//! - [`adaptive`] runs a turn on PAYG BYOK keys (inside) or an installed
//!   monthly-subscription coding CLI (outside), failing over between them,
//!   and names the Susi AI OS Socket endpoints the harness sits on.
//! - [`mcp`] speaks MCP to tool servers over stdio or the GMCP HTTP socket.
//! - [`author`] turns a roadmap vector into a validated plan through the
//!   metered gateway.
//! - [`supervisor`] runs a plan task by task, persisting after each one so an
//!   interrupted mission resumes instead of restarting.
//! - [`validate`] checks a plan against the repository: package names resolve,
//!   scopes live under an existing directory, acceptance is test-shaped.
//! - [`api`] is the command surface every client speaks.
//! - [`workflow`] is the embedded workflow engine: scope algebra, dependency
//!   and scope-disjoint wave scheduling, and an in-process, lock-free reader
//!   of the native coordinator's claims.
//! - [`verify`] checks the registry's backend ids against each provider's live
//!   model list, so an id retired upstream is reported before a call fails.
//!
//! The invariants the contracts keep:
//!
//! - Money never uses floating point; a ledger total is an integer sum.
//! - A price is recorded with the entry, so a later catalogue change cannot
//!   rewrite history.
//! - The event stream is append-only and ordered by `seq`; a client only
//!   renders it and never becomes a second source of truth.

pub mod adapter;
pub mod adaptive;
pub mod agents;
pub mod api;
pub mod author;
pub mod balance;
pub mod budget;
pub mod cache;
pub mod catalog;
#[cfg(windows)]
pub mod channel_bindings;
pub mod cockpit;
pub mod control;
pub mod event;
pub mod executor;
pub mod fleet;
pub mod gateway;
pub mod git_governor;
pub mod ingress;
pub mod ledger;
pub mod lifecycle;
pub mod mcp;
pub mod mission_control;
pub mod money;
pub mod operator;
pub mod policy;
pub mod pricing;
pub mod providers;
pub mod queue;
pub mod ratelimit;
pub mod recursive;
pub mod roi;
pub mod scheduler;
pub mod secrets;
pub mod smart;
pub mod storage_guard;
pub mod supervisor;
pub mod tools;
pub mod usage;
pub mod validate;
pub mod verify;
pub mod workflow;
pub mod workflow_runner;

/// The std HTTP/1.1 server the gateway, the ingress and the harness API
/// run on (it moved to `susi-http-transport`, shared with the A2A port and
/// the leaf services).
pub use susi_http_transport::server as http;

pub mod router;
pub mod worker;

#[cfg(test)]
#[path = "tests/http_wire.rs"]
mod http_wire;

pub use budget::BudgetReservation;
pub use ledger::{verify_integrity, LedgerIntegrityReport};
pub use supervisor::{
    build_adaptive_supervisor_report, discover_cli_fleet, discover_cli_fleet_in,
    discover_verified_container_contracts, discover_verified_container_contracts_in,
    execute_adaptive_task_with, execute_recursive_tree, execute_recursive_tree_with,
    AdaptiveExecutionPolicy, AdaptiveSupervisorReport, AdaptiveTaskOutcome, AdaptiveTaskRequest,
    CliAgentBackend, CliToolStatus, EmbeddedWorkflowOrchestrator, RecursiveTreeRun,
    SusiSocketReport, WorkflowEmbedError, WorkflowNode, WorkflowWavePlan,
};
pub use workflow_runner::{
    reconcile_worktree_with_remote, IsolatedWorkerWorkspace, ParallelAgentSpec,
    ParallelWorkflowHarness, ParallelWorkflowReport, RemoteReconcileConfig, RemoteSyncMode,
    WorkflowDistillSnapshot, WorkflowStep, WorkflowStepReceipt,
};

#[cfg(test)]
#[path = "tests/money.rs"]
mod inline_tests;
