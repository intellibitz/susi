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
//! shelling out to the CLI, an MCP client). Phase 0 defines the contracts
//! only; the supervisor loop, the metering proxy and the executor pool land in
//! later phases and consume these types.
//!
//! The split is deliberate:
//!
//! - [`money`] and [`pricing`] make cost arithmetic reproducible — integer
//!   micro-USD, prices per million tokens, cache hits priced far below misses.
//! - [`ledger`] is the append-only record of every attempt, priced at the rate
//!   in force when it ran, so spend can always be explained after the fact.
//! - [`event`] is the live, human-readable feed: one row per action, no code.
//! - [`api`] is the command surface every cockpit speaks.
//!
//! The invariants the contracts keep:
//!
//! - Money never uses floating point; a ledger total is an integer sum.
//! - A price is recorded with the entry, so a later catalogue change cannot
//!   rewrite history.
//! - The event stream is append-only and ordered by `seq`; a cockpit only
//!   renders it and never becomes a second source of truth.

pub mod api;
pub mod event;
pub mod ledger;
pub mod money;
pub mod pricing;
