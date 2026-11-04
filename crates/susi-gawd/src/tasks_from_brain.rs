//! Open tasks from brain evidence (unfit providers / missing defaults).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainSignal {
    pub provider: String,
    pub unfit_days: u32,
    pub default_model: Option<String>,
    pub default_served: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftTask {
    pub title: String,
    pub accept_hint: String,
}

/// When a provider stays unfit for days or a default model is not served.
#[must_use]
pub fn tasks_from_brain(signals: &[BrainSignal], unfit_threshold_days: u32) -> Vec<DraftTask> {
    let mut out = Vec::new();
    for s in signals {
        if s.unfit_days >= unfit_threshold_days {
            out.push(DraftTask {
                title: format!("Remediate unfit provider {}", s.provider),
                accept_hint: format!("cargo test -p susi-gemi provider_{}", s.provider),
            });
        }
        if let Some(model) = &s.default_model {
            if !s.default_served {
                out.push(DraftTask {
                    title: format!("Serve or remap default model {model}"),
                    accept_hint: format!("cargo test -p susi-gemi-models default_{model}"),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tasks_from_brain_tests {
    use super::*;

    #[test]
    fn tasks_from_brain_opens_for_unfit_and_unserved_default() {
        let drafts = tasks_from_brain(
            &[
                BrainSignal {
                    provider: "openai".into(),
                    unfit_days: 3,
                    default_model: None,
                    default_served: true,
                },
                BrainSignal {
                    provider: "local".into(),
                    unfit_days: 0,
                    default_model: Some("llama".into()),
                    default_served: false,
                },
            ],
            2,
        );
        assert_eq!(drafts.len(), 2);
        assert!(drafts[0].title.contains("openai"));
        assert!(drafts[1].title.contains("llama"));
    }

    #[test]
    fn tasks_from_brain_ignores_healthy() {
        assert!(tasks_from_brain(
            &[BrainSignal {
                provider: "x".into(),
                unfit_days: 1,
                default_model: Some("m".into()),
                default_served: true,
            }],
            2
        )
        .is_empty());
    }

    #[test]
    fn tasks_from_brain_returns_empty_for_empty_signals() {
        assert!(tasks_from_brain(&[], 1).is_empty());
    }

    #[test]
    fn tasks_from_brain_emits_both_tasks_when_single_signal_is_unfit_and_unserved() {
        let drafts = tasks_from_brain(
            &[BrainSignal {
                provider: "anthropic".into(),
                unfit_days: 5,
                default_model: Some("claude-sonnet".into()),
                default_served: false,
            }],
            5,
        );
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].title, "Remediate unfit provider anthropic");
        assert_eq!(
            drafts[0].accept_hint,
            "cargo test -p susi-gemi provider_anthropic"
        );
        assert_eq!(
            drafts[1].title,
            "Serve or remap default model claude-sonnet"
        );
        assert_eq!(
            drafts[1].accept_hint,
            "cargo test -p susi-gemi-models default_claude-sonnet"
        );
    }
}
