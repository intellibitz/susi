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
//! cockpit that drives it (the DeepSeek Harness panel, a browser, an agent
//! shelling out to the CLI, an MCP client). The supervisor loop and the
//! executor pool land in later phases; everything here is consumed by them.
//!
//! The split is deliberate:
//!
//! - [`money`] and [`pricing`] make cost arithmetic reproducible — integer
//!   micro-USD, prices per million tokens, cache hits priced far below misses.
//! - [`catalog`] holds the price data itself: a model the catalogue cannot
//!   price is refused at dispatch, because unpriced spend cannot be accounted.
//! - [`budget`] admits an attempt only when its worst-case estimate, the money
//!   already spent and the attempts already in flight all fit the mission cap.
//! - [`policy`] ranks models by expected cost per accepted task (learned from
//!   the ledger) and escalates on failure without retrying the same model.
//! - [`cockpit`] is the read-only status surface: spend per mission and the
//!   event feed, polled by the panel once a second.
//! - [`usage`] turns a provider's own JSON into [`pricing::TokenUsage`],
//!   including DeepSeek's and Gemini's cache-hit counts.
//! - [`ledger`] is the append-only record of every attempt, priced at the rate
//!   in force when it ran, so spend can always be explained after the fact.
//! - [`litellm`] is the model proxy behind the gateway: one OpenAI-compatible
//!   surface over DeepSeek, SiliconFlow, DashScope and Gemini, with caching.
//! - [`event`] is the live, human-readable feed: one row per action, no code.
//! - [`gateway`] meters a completion: forward it, price the provider's own
//!   usage, and append the ledger row before answering.
//! - [`executor`] runs one OpenClaw turn to a deadline, with a per-run key so
//!   the gateway attributes every call it makes.
//! - [`scheduler`] runs a mission's tasks concurrently, trying the candidate
//!   models in order and recording every attempt.
//! - [`adapter`] makes a susi target's own rules mechanical: worktree, claim,
//!   acceptance, finish.
//! - [`author`] turns a roadmap vector into a validated plan through the
//!   metered gateway.
//! - [`supervisor`] runs a plan task by task, persisting after each one so an
//!   interrupted mission resumes instead of restarting.
//! - [`validate`] checks a plan against the repository: package names resolve,
//!   scopes live under an existing directory, acceptance is test-shaped.
//! - [`api`] is the command surface every cockpit speaks.
//!
//! The invariants the contracts keep:
//!
//! - Money never uses floating point; a ledger total is an integer sum.
//! - A price is recorded with the entry, so a later catalogue change cannot
//!   rewrite history.
//! - The event stream is append-only and ordered by `seq`; a cockpit only
//!   renders it and never becomes a second source of truth.

pub mod adapter;
pub mod api;
pub mod author;
pub mod budget;
pub mod catalog;
pub mod cockpit;
pub mod control;
pub mod event;
pub mod executor;
pub mod gateway;
pub mod ledger;
pub mod litellm;
pub mod money;
pub mod policy;
pub mod pricing;
pub mod scheduler;
pub mod supervisor;
pub mod usage;
pub mod validate;
