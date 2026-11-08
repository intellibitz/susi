#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Dependency-light `clap` CLI surface definitions shared by `susi` and the
//! `susi-cli` thin client (`D-SP-4`).
//!
//! This crate intentionally carries no package version attribute on the root
//! parser: each binary attaches its own `env!("CARGO_PKG_VERSION")` at parse
//! time via `Cli::command().version(env!("CARGO_PKG_VERSION"))`, so `--version`
//! always reports the running executable's package version.

/// `susi agents`: external task executor commands.
pub mod agent;
/// `susi ambient`: ambient context synchronization commands.
pub mod ambient;
/// `susi auto`: zero-config auto substrate commands.
pub mod auto;
/// `susi blackboard`: mission blackboard inspection commands.
pub mod blackboard;
/// `susi brain`: evidence-ranked model routing commands.
pub mod brain;
/// `susi broker`: inter-app permission and message broker commands.
pub mod broker;
/// `susi commits`: replicated quorum-commit ledger audit commands.
pub mod commits;
/// `susi context-graph`: universal context graph commands.
pub mod context_graph;
/// `susi crown`: Tier S USP verification commands.
pub mod crown;
/// `susi ecosystem`: local AI ecosystem and knowledge base commands.
pub mod ecosystem;
/// `susi extensions`: extension pack commands.
pub mod extensions;
/// `susi frameworks`: agent framework and engine commands.
pub mod framework;
/// `susi frontier`: top frontier model commands.
pub mod frontier;
/// `susi harness`: plan, mission and harness API commands.
pub mod harness;
/// `susi intent`: semantic intent bus commands.
pub mod intent;
/// `susi mcp`: leading MCP server and native MCP endpoint commands.
pub mod mcp;
/// `susi model`: coding/agent model control plane commands.
pub mod model;
/// `susi open-weight`: open-weight frontier model commands.
pub mod open_weight;
/// `susi openrouter`: OpenRouter control plane commands.
pub mod openrouter;
/// `susi os`: operating-system substrate view and lifecycle commands.
pub mod os;
/// `susi patch`: patch apply-and-test arguments.
pub mod patch;
/// `susi peers`: verified cluster roster commands.
pub mod peers;
/// `susi plan`: autonomous planning loop arguments.
pub mod plan;
/// `susi privacy`: privacy mode and capability grant commands.
pub mod privacy;
/// `susi run`: direct streaming model execution arguments.
pub mod run;
/// `susi services`: leaf-service process table commands.
pub mod services;
/// `susi setup`: zero-config ecosystem setup commands.
pub mod setup;
/// `susi substrate`: Tier S substrate status commands.
pub mod substrate;
/// `susi tasks`: what stays in core of the former task-queue family (`eval-corpus`).
pub mod tasks;
/// `susi telemetry`: host telemetry commands.
pub mod telemetry;
/// `susi top`: operator control surface arguments.
pub mod top;
/// `susi tx`: multi-agent transaction commands.
pub mod tx;
