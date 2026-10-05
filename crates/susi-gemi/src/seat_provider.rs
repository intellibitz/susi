//! Agent-seat provider (VC-202-022): a configured seat registers as an
//! ordinary `Provider` named `seat-<agent>`, so the whole brain — ranking,
//! election, capability matrix, routing ladder, key arbitration and spend
//! ceilings — treats it exactly like a model API. `generate` delegates the
//! prompt through the existing `agents.external.managed` capability-bus
//! path: the managed catalogue resolves the seat's adapter, a durable run
//! is prepared and executed, and the run's captured output is the answer.
//!
//! A seat carries its own `KeyArbiter` built from its `SeatSpec`: the
//! subscription cap is a quota window, the per-seat concurrency and rate
//! bounds are arbiter limits. Exhaustion surfaces as a quota/rate denial
//! — the cascade records the rung step-down and tries the next worker
//! rather than hammering a spent seat.

use std::any::Any;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::susi_core::provider::BoxFuture;
use crate::worker::{seat_name, SeatSpec};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A subscription-billed agent seat exposed as a brain candidate.
pub struct SeatProvider {
    spec: SeatSpec,
    arbiter: std::sync::Arc<crate::key_arbitration::KeyArbiter>,
    name: String,
}

impl SeatProvider {
    pub fn new(spec: SeatSpec) -> Self {
        let name = seat_name(&spec.agent);
        let arbiter = std::sync::Arc::new(crate::key_arbitration::KeyArbiter::new(
            crate::key_arbitration::ArbiterLimits {
                per_key_concurrent: spec.max_concurrent,
                // The per-seat rate bound, expressed in the arbiter's
                // sliding window: rate_per_min over 60s.
                per_key_requests: spec.rate_per_min,
                window_secs: 60,
                global_concurrent: u32::MAX,
                // The subscription cap IS a quota window: spend it and the
                // seat refuses until reset — a Denial, not a hammered API.
                quota: vec![crate::key_arbitration::QuotaWindowSpec {
                    kind: crate::key_arbitration::WindowKind::Rolling {
                        period_secs: spec.window_secs,
                    },
                    allowance: spec.calls_per_window,
                }],
            },
        ));
        Self {
            spec,
            arbiter,
            name,
        }
    }

    /// Observable seat state for [`crate::worker::descriptor`]-level views.
    pub fn quota_headroom(&self) -> Option<(u64, u64)> {
        self.arbiter.quota_headroom(&self.name)
    }
}

fn deny(kind: &str, detail: &str) -> crate::susi_core::susi_error::EaiError {
    // "rate limit"/"quota" wording classifies as RateLimit on the health
    // axis (classify_error), so a spent seat cools instead of being
    // retried inside its exhausted window.
    crate::susi_core::susi_error::EaiError::network(format!("{kind}: {detail}"))
}

impl crate::susi_core::provider::Provider for SeatProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<bool>> {
        // A saturated seat is not dead — it is unavailable for new work
        // until its window resets; the descriptor still reports it.
        let scope = self.name.clone();
        let arbiter = &self.arbiter;
        Box::pin(async move {
            Ok(arbiter
                .quota_headroom(&scope)
                .map(|(r, _)| r > 0)
                .unwrap_or(true))
        })
    }

    fn generate(
        &self,
        prompt: &str,
    ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<String>> {
        let name = self.name.clone();
        let agent = self.spec.agent.clone();
        let arbiter = self.arbiter.clone();
        let prompt_text = prompt.to_string();
        Box::pin(async move {
            let scope = name.clone();
            let permit = arbiter.try_acquire(&scope);
            let _permit = permit.map_err(|d| match d {
                crate::key_arbitration::Denial::Quota { reset_unix } => deny(
                    "quota",
                    &format!("seat subscription window spent — resets at unix {reset_unix}"),
                ),
                crate::key_arbitration::Denial::Concurrency
                | crate::key_arbitration::Denial::RateWindow
                | crate::key_arbitration::Denial::Global => deny("rate limit", d.describe()),
            })?;
            crate::worker::record_seat_call(&name, now_unix());
            let agent_name = agent.clone();
            tokio::task::spawn_blocking(move || {
                crate::susi_core::plane_bus::agents::external_managed_goal(
                    &agent_name,
                    &prompt_text,
                    &std::path::PathBuf::from("."),
                )
            })
            .await
            .map_err(|e| {
                crate::susi_core::susi_error::EaiError::process(format!(
                    "seat worker '{name}': {e}"
                ))
            })?
            .map_err(crate::susi_core::susi_error::EaiError::network)
        })
    }

    fn embed(
        &self,
        _text: &str,
    ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<Vec<f32>>> {
        Box::pin(async move {
            Err(crate::susi_core::susi_error::EaiError::config(
                "agent seats answer prompts; they do not embed".to_string(),
            ))
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Register every configured seat as a provider. Called from the same
/// production registration point as cloud endpoints, so a declared seat
/// joins every candidate set the brain enumerates.
pub fn register_configured_seats(registry: &crate::susi_core::registry::CapabilityRegistry) {
    for spec in crate::worker::configured_seats() {
        let name = seat_name(&spec.agent);
        if registry.get_provider(&name).is_none() {
            registry.register_provider(SeatProvider::new(spec));
        }
    }
}
