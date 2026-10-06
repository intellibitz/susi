//! Test for model health state machine with typed failure classes (T-DEEPSEEK-115).
//! Verifies proper state transitions based on failure types.

use std::collections::HashMap;

/// Typed failure classes for provider responses and transport errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// Authentication rejected
    AuthRejected,
    /// Rate limited
    RateLimited,
    /// Request timeout
    Timeout,
    /// Server error (5xx)
    ServerError,
    /// Context overflow / token limit exceeded
    ContextOverflow,
    /// Content filtered
    ContentFiltered,
    /// Malformed response
    Malformed,
}

/// Health states for models and API keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Healthy and available
    Healthy,
    /// Cooling down until deadline (e.g., rate limit window)
    Cooling { until_unix: u64 },
    /// Degraded but may recover
    Degraded,
    /// Key is dead (auth failure)
    KeyDead,
    /// Out of quota
    OutOfQuota,
}

/// Per-model health tracker with state machine transitions.
pub struct ModelHealth {
    model_id: String,
    state: HealthState,
    failure_history: Vec<FailureClass>,
}

impl ModelHealth {
    pub fn new(model_id: &str) -> Self {
        Self {
            model_id: model_id.to_string(),
            state: HealthState::Healthy,
            failure_history: Vec::new(),
        }
    }

    /// Record a failure and transition state if needed.
    pub fn record_failure(&mut self, failure: FailureClass, current_unix: u64) {
        self.failure_history.push(failure);

        // State transitions based on failure class
        match failure {
            FailureClass::AuthRejected => {
                self.state = HealthState::KeyDead;
            }
            FailureClass::RateLimited => {
                // Rate limits are availability, not capability
                // Cool down for 60 seconds (typical Retry-After)
                self.state = HealthState::Cooling {
                    until_unix: current_unix + 60,
                };
            }
            FailureClass::Timeout => {
                // Timeout is availability, not capability
                self.state = HealthState::Cooling {
                    until_unix: current_unix + 30,
                };
            }
            FailureClass::ServerError => {
                // Server error is transient
                self.state = HealthState::Degraded;
            }
            FailureClass::ContextOverflow => {
                // Context overflow is capability, not availability
                // Mark as degraded but not dead
                self.state = HealthState::Degraded;
            }
            FailureClass::ContentFiltered => {
                // Content filtered is not a model failure
                // Keep state but don't escalate
            }
            FailureClass::Malformed => {
                // Malformed response is a bug, mark degraded
                self.state = HealthState::Degraded;
            }
        }
    }

    pub fn state(&self) -> HealthState {
        self.state
    }

    pub fn failure_count(&self, class: FailureClass) -> usize {
        self.failure_history.iter().filter(|&&f| f == class).count()
    }

    /// Check if model is ready given current time
    pub fn is_ready(&self, current_unix: u64) -> bool {
        match self.state {
            HealthState::Healthy => true,
            HealthState::Cooling { until_unix } => current_unix >= until_unix,
            HealthState::Degraded => false,
            HealthState::KeyDead => false,
            HealthState::OutOfQuota => false,
        }
    }
}

/// Registry of model health states
pub struct HealthRegistry {
    models: HashMap<String, ModelHealth>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
        }
    }

    pub fn get_or_create(&mut self, model_id: &str) -> &mut ModelHealth {
        self.models
            .entry(model_id.to_string())
            .or_insert_with(|| ModelHealth::new(model_id))
    }

    pub fn get_health(&self, model_id: &str) -> Option<HealthState> {
        self.models.get(model_id).map(|h| h.state())
    }
}

#[test]
fn model_health_failure_classes() {
    // Foundation test: verify typed failure classes and state machine.
    // In production, this will:
    // 1. Classify every error into a typed class
    // 2. Drive state machine: healthy -> cooling/degraded/dead
    // 3. Separate availability (timeout/rate-limit) from capability (overflow)
    // 4. Make health observable and restart-persistent
    // 5. Prevent wrong classifications (timeout != capability failure)

    let mut registry = HealthRegistry::new();
    let now = 1000u64;

    // Test 1: Auth rejection → KeyDead
    let claude = registry.get_or_create("claude-3-sonnet");
    claude.record_failure(FailureClass::AuthRejected, now);
    assert_eq!(claude.state(), HealthState::KeyDead);
    assert!(!claude.is_ready(now));

    // Test 2: Rate limit → Cooling state
    let gpt4 = registry.get_or_create("gpt-4-turbo");
    gpt4.record_failure(FailureClass::RateLimited, now);
    assert_eq!(
        gpt4.state(),
        HealthState::Cooling {
            until_unix: now + 60
        }
    );
    assert!(!gpt4.is_ready(now));
    assert!(gpt4.is_ready(now + 61)); // Ready after cooldown

    // Test 3: Timeout → Cooling, not capability failure
    let deepseek = registry.get_or_create("deepseek-coder");
    deepseek.record_failure(FailureClass::Timeout, now);
    assert_eq!(
        deepseek.state(),
        HealthState::Cooling {
            until_unix: now + 30
        }
    );
    assert_eq!(deepseek.failure_count(FailureClass::Timeout), 1);
    // Timeout should not affect capability - verify it's not marked degraded
    assert!(!deepseek.is_ready(now)); // Still cooling

    // Test 4: Context overflow → Degraded (capability, not availability)
    let llama = registry.get_or_create("llama-70b");
    llama.record_failure(FailureClass::ContextOverflow, now);
    assert_eq!(llama.state(), HealthState::Degraded);
    assert!(!llama.is_ready(now)); // Degraded, not available

    // Test 5: Server error → Degraded (transient)
    let mistral = registry.get_or_create("mistral-large");
    mistral.record_failure(FailureClass::ServerError, now);
    assert_eq!(mistral.state(), HealthState::Degraded);

    // Test 6: Content filtered → stays healthy (not model failure)
    let openai = registry.get_or_create("gpt-3.5-turbo");
    openai.record_failure(FailureClass::ContentFiltered, now);
    assert_eq!(openai.state(), HealthState::Healthy);
    assert!(openai.is_ready(now));

    // Test 7: Multiple failures tracked separately
    let multi = registry.get_or_create("multi-fail");
    multi.record_failure(FailureClass::RateLimited, now);
    multi.record_failure(FailureClass::ServerError, now + 100);
    multi.record_failure(FailureClass::Timeout, now + 200);
    assert_eq!(multi.failure_count(FailureClass::RateLimited), 1);
    assert_eq!(multi.failure_count(FailureClass::ServerError), 1);
    assert_eq!(multi.failure_count(FailureClass::Timeout), 1);

    // Test 8: Registry maintains separate health per model
    assert_eq!(
        registry.get_health("claude-3-sonnet"),
        Some(HealthState::KeyDead)
    );
    assert_eq!(
        registry.get_health("gpt-4-turbo"),
        Some(HealthState::Cooling {
            until_unix: now + 60
        })
    );
    assert_eq!(registry.get_health("unknown-model"), None);

    // Summary: typed failure classes enable proper state management.
    // Full implementation will:
    // - Parse provider errors into typed classes
    // - Store health state durably with timestamps
    // - Separate availability from capability concerns
    // - Make health observable (metrics, dashboards)
    // - Support recovery: retry after cooldown, manual recovery
    // - Export failure stats for analysis
}
