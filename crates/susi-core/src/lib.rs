//! # susi-core
//!
//! susi is a zero-trust AI operating system. It is infinitely Pluggable (4)—bring any
//! model, any agent, and any MCP tool. We orchestrate them into a consensus-driven
//! Swarm (1). But unlike naive frameworks, susi operates on strict Evidence (2).
//! Our Universal Truth (3) Transformer cross-examines every claim to physically
//! and semantically eliminate hallucinations. Every action is Cryptographically
//! Audited (5) for compliance, untrusted code is tightly Sandboxed (6) for security,
//! routine intelligence is compiled into fast Reflexes (7) for speed, and missing
//! tools are Autonomously Provisioned (8) on the fly.

pub mod bus;
pub mod evidence;
pub mod manifold;
pub mod provider;
pub mod queue;
pub mod redact;
pub mod registry;
pub mod truth;

// Top-level exports for the fundamental susi-core types
pub use evidence::{Claim, EvidenceRecord, EvidenceSource};
pub use provider::Provider;
pub use registry::{CapabilityRegistry, Tool};
pub use truth::TruthTransformer;
