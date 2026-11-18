//! Capability floor for provider ranking (VC-202-004).
//!
//! Each task class declares the minimum model capability its work needs
//! ([`super::brain::TaskClass::capability_floor`]); a provider's capability
//! is looked up here by name through an editable rule list, exactly like the
//! cost tiers in [`super::cost`]. The floor is checked before the price is
//! compared, so `SUSI_BUDGET=low` pressure can never send hard work to a
//! weak model: a below-floor candidate sorts behind every floor-meeting one
//! regardless of its cost or evidence.

use serde::Deserialize;
use std::sync::OnceLock;

/// What a model can be trusted with, coarsely: `Basic < Coding < Reasoning`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    /// Short, simple turns — summarization, classification, chat.
    Basic,
    /// Code generation, analysis and refactoring.
    Coding,
    /// Deep multi-step reasoning, proofs, architecture.
    Reasoning,
}

impl ModelCapability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Coding => "coding",
            Self::Reasoning => "reasoning",
        }
    }
}

#[derive(Debug, Deserialize)]
struct Rule {
    contains: String,
    capability: ModelCapability,
}

#[derive(Debug, Default, Deserialize)]
struct Rules {
    #[serde(default)]
    rules: Vec<Rule>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let user = susi_paths::SusiDirs::config_dir().join("capability_tiers.json");
        std::fs::read_to_string(user)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            // The bundled file is compiled in; a parse failure is caught by
            // `bundled_rules_parse_and_classify_known_names` in any test run.
            .or_else(|| {
                serde_json::from_str(include_str!("../../../../config/capability-tiers.json")).ok()
            })
            .unwrap_or_default()
    })
}

/// Capability of a provider by name. Rules are checked in order against the
/// lowercase name and apply to cloud and local names alike — a local
/// `qwen2.5-coder` is a coding model too. A cloud name with no hit is
/// assumed `Coding` (the middle of the ladder, like cost's `mid` default);
/// a local name with no hit is `Basic` — an unprofiled small model is the
/// conservative floor.
pub fn of(provider: &str) -> ModelCapability {
    let name = provider.to_ascii_lowercase();
    if let Some(cap) = rules()
        .rules
        .iter()
        .find(|r| name.contains(&r.contains.to_ascii_lowercase()))
        .map(|r| r.capability)
    {
        return cap;
    }
    if super::routing::InferenceRouter::is_cloud_provider_name(provider) {
        ModelCapability::Coding
    } else {
        ModelCapability::Basic
    }
}

/// True when `provider` meets the floor `class` declares. This is the check
/// that runs before price: membership is capability-only, so no budget dial
/// or success streak can push a weak model above a floor-meeting one.
pub fn meets_floor(provider: &str, class: super::brain::TaskClass) -> bool {
    of(provider) >= class.capability_floor()
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use crate::engines::brain::TaskClass;

    #[test]
    fn model_capability_ordering_and_labels_follow_basic_coding_reasoning_hierarchy() {
        assert!(ModelCapability::Basic < ModelCapability::Coding);
        assert!(ModelCapability::Coding < ModelCapability::Reasoning);
        assert_eq!(ModelCapability::Basic.label(), "basic");
        assert_eq!(ModelCapability::Coding.label(), "coding");
        assert_eq!(ModelCapability::Reasoning.label(), "reasoning");
    }

    #[test]
    fn of_classifies_bundled_reasoning_and_coding_provider_names() {
        assert_eq!(of("claude-opus-4"), ModelCapability::Reasoning);
        assert_eq!(of("deepseek-r1"), ModelCapability::Reasoning);
        assert_eq!(of("qwen2.5-coder"), ModelCapability::Coding);
    }

    #[test]
    fn of_defaults_unknown_local_provider_to_basic() {
        assert_eq!(of("unknown-tiny-local-model-xyz"), ModelCapability::Basic);
    }

    #[test]
    fn meets_floor_enforces_task_class_capability_minimums() {
        assert!(meets_floor("claude-opus-4", TaskClass::Reasoning));
        assert!(!meets_floor(
            "unknown-tiny-local-model-xyz",
            TaskClass::Code
        ));
        assert!(meets_floor(
            "unknown-tiny-local-model-xyz",
            TaskClass::Reflex
        ));
    }

    #[test]
    fn coding_tier_satisfies_code_and_reflex_floors_but_not_reasoning() {
        assert!(meets_floor("qwen2.5-coder", TaskClass::Reflex));
        assert!(meets_floor("qwen2.5-coder", TaskClass::Code));
        assert!(!meets_floor("qwen2.5-coder", TaskClass::Reasoning));
        assert!(meets_floor("deepseek-r1", TaskClass::Code));
    }
}
