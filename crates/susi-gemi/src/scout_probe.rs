//! Scout probe: a deterministic micro-benchmark a candidate model can be
//! run against at negligible cost (VC-202-003 / T-DEEPSEEK-99).
//!
//! The probe set is fixed and tiny — a handful of short prompts whose
//! correct answers are checkable *offline* by plain string predicates, so a
//! probe measures the model, never the network and never a second model
//! acting as judge. Every candidate faces the identical set, which is what
//! makes results comparable across models and across days: drift between
//! two runs of the same provider is the signal the scheduled scout
//! (T-DEEPSEEK-107) reports on.
//!
//! Verified probe outcomes feed the same evidence the live cascade records
//! (`brain::record_outcome`), so a probed provider's cost per verified
//! outcome in `Store::rank` reflects the measurement — and a provider that
//! cannot admit through the key arbiter, or answers flattened error text,
//! records the failure instead of silently leading (Mandate 56).

use crate::engines::brain::{self, TaskClass};
use serde::Serialize;

/// One fixed probe: a short prompt and an offline-checkable correctness
/// predicate. `expected` is human-facing documentation for reports; the
/// check is the verdict.
pub struct ScoutProbe {
    pub class: TaskClass,
    pub prompt: &'static str,
    pub expected: &'static str,
    pub check: fn(&str) -> bool,
}

fn has_word(answer: &str, word: &str) -> bool {
    answer
        .split(|c: char| !c.is_alphanumeric())
        .any(|tok| tok.eq_ignore_ascii_case(word))
}

fn ok_literal(a: &str) -> bool {
    a.trim().eq_ignore_ascii_case("ok")
}
fn has_15(a: &str) -> bool {
    has_word(a, "15")
}
fn has_tokyo(a: &str) -> bool {
    has_word(a, "tokyo")
}
fn has_primary_color(a: &str) -> bool {
    has_word(a, "red") || has_word(a, "blue") || has_word(a, "yellow")
}
fn has_let(a: &str) -> bool {
    has_word(a, "let")
}
fn has_carol(a: &str) -> bool {
    has_word(a, "carol")
}

/// The fixed daily probe set. Bounded on purpose: six prompts of a few
/// tokens each keep a full probe at negligible cost — safe to run daily.
/// Every check is a deterministic string predicate, never a model judge,
/// so scores are comparable across providers and over time.
pub const SCOUT_PROBES: &[ScoutProbe] = &[
    ScoutProbe {
        class: TaskClass::Reflex,
        prompt: "Reply with exactly: OK",
        expected: "OK",
        check: ok_literal,
    },
    ScoutProbe {
        class: TaskClass::Reflex,
        prompt: "What is 7 plus 8? Reply with the number only.",
        expected: "15",
        check: has_15,
    },
    ScoutProbe {
        class: TaskClass::Chat,
        prompt: "In one word, what is the capital of Japan?",
        expected: "tokyo",
        check: has_tokyo,
    },
    ScoutProbe {
        class: TaskClass::Chat,
        prompt: "Name one primary color. One word only.",
        expected: "red | blue | yellow",
        check: has_primary_color,
    },
    ScoutProbe {
        class: TaskClass::Code,
        prompt: "In Rust, which keyword declares an immutable variable? Reply with the keyword only.",
        expected: "let",
        check: has_let,
    },
    ScoutProbe {
        class: TaskClass::Reasoning,
        prompt: "Alice is taller than Bob. Bob is taller than Carol. Who is the shortest? Reply with the name only.",
        expected: "carol",
        check: has_carol,
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub class: TaskClass,
    pub prompt: &'static str,
    pub expected: &'static str,
    /// Bounded preview of what the model actually said (full answers are
    /// provider output, not for the report).
    pub answer_preview: Option<String>,
    pub correct: bool,
    pub latency_ms: u64,
    /// Transport/admission failure — distinct from a wrong answer.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoutReport {
    pub provider: String,
    pub probes: usize,
    /// Probes that ran *and* answered correctly.
    pub verified: usize,
    /// Probes that never produced an answer (arbiter denial, HTTP error,
    /// flattened error text).
    pub transport_failures: usize,
    pub results: Vec<ProbeResult>,
}

impl ScoutReport {
    /// Fraction of attempted probes verified — `None` when every probe hit
    /// a transport failure and there is nothing to score.
    pub fn verified_fraction(&self) -> Option<f32> {
        let attempted = self.probes - self.transport_failures;
        (attempted > 0).then(|| self.verified as f32 / attempted as f32)
    }

    /// A report for a provider that could not be probed at all — the name
    /// did not resolve, or the sweep failed before a prompt was sent.
    /// Every probe counts as a transport failure so drift detection sees
    /// the death the same way as a dead key.
    #[must_use]
    pub fn transport_dead(provider: &str, error: String) -> Self {
        let results = SCOUT_PROBES
            .iter()
            .map(|probe| ProbeResult {
                class: probe.class,
                prompt: probe.prompt,
                expected: probe.expected,
                answer_preview: None,
                correct: false,
                latency_ms: 0,
                error: Some(error.clone()),
            })
            .collect::<Vec<_>>();
        Self {
            provider: provider.to_string(),
            probes: results.len(),
            verified: 0,
            transport_failures: results.len(),
            results,
        }
    }
}

const ANSWER_PREVIEW_CHARS: usize = 120;

/// Run the fixed probe set through an injected `complete` — the seam that
/// keeps the harness hermetic in tests and drives the real provider in
/// production. `complete` gets the prompt verbatim; transport errors come
/// back as `Err` and count as transport failures, not wrong answers.
pub fn probe_with(
    provider: &str,
    complete: &dyn Fn(&str) -> Result<String, String>,
) -> ScoutReport {
    let mut results = Vec::with_capacity(SCOUT_PROBES.len());
    for probe in SCOUT_PROBES {
        let started = std::time::Instant::now();
        let outcome = complete(probe.prompt);
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (answer, error) = match outcome {
            Ok(text) if text.trim().is_empty() => (None, Some("empty answer".to_string())),
            Ok(text) if crate::engines::runtime::GemiEngine::looks_like_error_text(&text) => {
                (None, Some("flattened error text".to_string()))
            }
            Ok(text) => (Some(text), None),
            Err(e) => (None, Some(e)),
        };
        let correct = answer.as_deref().is_some_and(|a| (probe.check)(a));
        results.push(ProbeResult {
            class: probe.class,
            prompt: probe.prompt,
            expected: probe.expected,
            answer_preview: answer
                .as_deref()
                .map(|a| a.chars().take(ANSWER_PREVIEW_CHARS).collect()),
            correct,
            latency_ms,
            error,
        });
    }
    let verified = results.iter().filter(|r| r.correct).count();
    let transport_failures = results.iter().filter(|r| r.error.is_some()).count();
    ScoutReport {
        provider: provider.to_string(),
        probes: results.len(),
        verified,
        transport_failures,
        results,
    }
}

/// Production probe: resolve `name` in the configured provider registry and
/// drive it through the same admission the dispatch cascade uses — per-key
/// rate/concurrency/quota arbitration first, then the provider's own
/// `generate`. A provider that cannot admit, or replies flattened error
/// text, records probe failures instead of scoring.
pub fn probe_registered(provider_name: &str) -> Result<ScoutReport, String> {
    crate::engines::http_provider::register_configured_cloud_endpoints(
        crate::susi_core::registry::CapabilityRegistry::global(),
    );
    let registry = crate::susi_core::registry::CapabilityRegistry::global();
    let provider = registry
        .get_provider(provider_name)
        .ok_or_else(|| format!("{provider_name} is not a registered provider"))?;
    let runtime = crate::engines::runtime::GemiEngine::provider_runtime()
        .ok_or_else(|| "provider runtime unavailable".to_string())?;
    Ok(probe_with(provider_name, &|prompt| {
        let _permit = crate::key_arbitration::try_acquire(provider_name)
            .map_err(|d| d.describe().to_string())?;
        runtime
            .block_on(provider.generate(prompt))
            .map_err(|e| e.to_string())
    }))
}

/// Record every probe outcome as brain evidence under its task class — a
/// correct answer is a verified useful outcome, a wrong answer or a
/// transport failure is a recorded failure. Returns the number of outcomes
/// recorded; the ranking reads them on the next `Store::rank`.
pub fn record_outcomes(report: &ScoutReport) -> usize {
    let mut n = 0;
    for r in &report.results {
        brain::record_outcome(&report.provider, r.class, r.correct, r.latency_ms);
        n += 1;
    }
    n
}
