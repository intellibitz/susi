//! Intent-derived verification for synthesized reflexes (VC-200-002).
//!
//! A probe run proves a model-written reflex *executes*; it says nothing
//! about whether it is *right*. Registration now requires the intent to
//! carry executable checks — input → expected-output fixtures, plus
//! VC-201-008 property/invariant checks when the intent provides them —
//! all run against the staged `.wasm` under the WASI sandbox before the
//! live name is published. A reflex that fails any check is never
//! registered and the capability gap stays open (the caller's probe
//! reflex reports `functional: false`, as before).

use serde::{Deserialize, Serialize};

/// One intent-derived fixture: run the reflex on `input`, expect the
/// declared output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentCheck {
    pub input: String,
    /// Exact trimmed stdout equality. Either this or
    /// `expect_contains` must be present — a fixture with no
    /// expectation verifies nothing.
    #[serde(default)]
    pub expected: Option<String>,
    /// Trimmed stdout must contain this substring — for reflexes whose
    /// output embeds the result in a report line.
    #[serde(default)]
    pub expect_contains: Option<String>,
}

/// A VC-201-008 property check the intent can declare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InvariantSpec {
    /// Two inputs equivalent under whitespace normalization must
    /// produce identical output (metamorphic).
    WhitespaceNormalize { a: String, b: String },
    /// Every observed output must stay inside this allowlist.
    OutputAllowlist { allowed: Vec<String> },
}

/// The intent's executable check set, extracted from the model's
/// fixtures block or supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentSpec {
    #[serde(default)]
    pub fixtures: Vec<IntentCheck>,
    #[serde(default)]
    pub invariants: Vec<InvariantSpec>,
}

/// Parse the model's fixtures block — a bare `IntentSpec` JSON object.
/// `None` when the block is absent, malformed, or carries no
/// executable check: an intent with nothing to verify against is not
/// verifiable, so `synthesize_capability_for` refuses registration.
#[must_use]
pub fn parse_intent_spec(json: &str) -> Option<IntentSpec> {
    let spec: IntentSpec = serde_json::from_str(json).ok()?;
    let real_fixtures = spec
        .fixtures
        .iter()
        .all(|f| !f.input.is_empty() && (f.expected.is_some() || f.expect_contains.is_some()));
    if !real_fixtures {
        return None;
    }
    (!spec.fixtures.is_empty() || !spec.invariants.is_empty()).then_some(spec)
}

/// Run every intent check against the staged module via `run`
/// (`execute_reflex` in production — the WASI sandbox).
///
/// # Errors
/// The first failing check, named with the evidence — the caller keeps
/// the reflex unregistered and the gap open.
pub fn run_checks(
    spec: &IntentSpec,
    run: &dyn Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    for f in &spec.fixtures {
        let out = run(&f.input)
            .map_err(|e| format!("fixture {:?}: reflex did not execute: {e}", f.input))?;
        let trimmed = out.trim();
        if let Some(expected) = &f.expected {
            if trimmed != expected.trim() {
                return Err(format!(
                    "fixture {:?}: expected {expected:?}, got {trimmed:?}",
                    f.input
                ));
            }
        }
        if let Some(needle) = &f.expect_contains {
            if !trimmed.contains(needle.trim()) {
                return Err(format!(
                    "fixture {:?}: output {trimmed:?} lacks expected substring {needle:?}",
                    f.input
                ));
            }
        }
    }
    for inv in &spec.invariants {
        match inv {
            InvariantSpec::WhitespaceNormalize { a, b } => {
                let left = run(a).map_err(|e| format!("invariant whitespace: run failed: {e}"))?;
                let right = run(b).map_err(|e| format!("invariant whitespace: run failed: {e}"))?;
                if left.trim() != right.trim() {
                    return Err(format!(
                        "invariant whitespace_normalize fails: {a:?} → {left:?} but {b:?} → {right:?}"
                    ));
                }
            }
            InvariantSpec::OutputAllowlist { allowed } => {
                for f in &spec.fixtures {
                    let out = run(&f.input).map_err(|e| format!("allowlist probe failed: {e}"))?;
                    let trimmed = out.trim().to_string();
                    if !allowed.iter().any(|a| a == &trimmed) {
                        return Err(format!(
                            "output {trimmed:?} outside declared allowlist {allowed:?}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
