//! Cost awareness for provider ranking. Prices change too often to bake in,
//! so providers are placed in coarse marginal-cost tiers (`free < low < mid <
//! high`) by an editable rule list, and a budget dial decides how much a
//! tier step costs a candidate — a lot for reflex-sized work, almost nothing
//! for hard reasoning, where answer quality outweighs the bill.
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostTier {
    Free,
    Low,
    Mid,
    High,
}

impl CostTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Low => "low",
            Self::Mid => "mid",
            Self::High => "high",
        }
    }

    fn steps(self) -> f32 {
        match self {
            Self::Free => 0.0,
            Self::Low => 1.0,
            Self::Mid => 2.0,
            Self::High => 3.0,
        }
    }
}

/// How hard the router leans on cost. `SUSI_BUDGET=low|balanced|max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// Cost matters twice as much: prefer cheaper providers more aggressively.
    Low,
    Balanced,
    /// Ignore cost; pure quality/evidence ordering.
    Max,
}

impl Budget {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "frugal" => Some(Self::Low),
            "balanced" | "" => Some(Self::Balanced),
            "max" | "unlimited" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        crate::susi_config::env_or_cloud_env("SUSI_BUDGET")
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or(Self::Balanced)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Balanced => "balanced",
            Self::Max => "max",
        }
    }

    fn weight(self) -> f32 {
        match self {
            Self::Low => 2.0,
            Self::Balanced => 1.0,
            Self::Max => 0.0,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Rule {
    contains: String,
    tier: CostTier,
}

#[derive(Debug, Default, Deserialize)]
struct Rules {
    #[serde(default)]
    rules: Vec<Rule>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let user = susi_paths::SusiDirs::config_dir().join("cost_tiers.json");
        std::fs::read_to_string(user)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            // The bundled file is compiled in; a parse failure is caught by
            // `bundled_rules_parse_and_classify_known_names` in any test run.
            .or_else(|| {
                serde_json::from_str(include_str!("../../../../config/cost-tiers.json")).ok()
            })
            .unwrap_or_default()
    })
}

fn tier_with(rules: &Rules, provider: &str, cloud: bool) -> CostTier {
    if !cloud {
        return CostTier::Free;
    }
    let name = provider.to_ascii_lowercase();
    rules
        .rules
        .iter()
        .find(|r| name.contains(&r.contains.to_ascii_lowercase()))
        .map_or(CostTier::Mid, |r| r.tier)
}

/// Marginal-cost tier of a provider by name (local engines are free).
pub fn tier_of(provider: &str) -> CostTier {
    tier_with(
        rules(),
        provider,
        super::routing::InferenceRouter::is_cloud_provider_name(provider),
    )
}

/// Score penalty for running `class` work on a provider of `tier`.
pub fn penalty(tier: CostTier, class: super::brain::TaskClass, budget: Budget) -> f32 {
    use super::brain::TaskClass;
    let per_step = match class {
        TaskClass::Reflex => 0.04,
        TaskClass::Chat => 0.03,
        TaskClass::Code => 0.012,
        TaskClass::Reasoning => 0.004,
    };
    tier.steps() * per_step * budget.weight()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::brain::TaskClass;

    fn bundled() -> Rules {
        serde_json::from_str(include_str!("../../../../config/cost-tiers.json")).unwrap()
    }

    #[test]
    fn bundled_rules_parse_and_classify_known_names() {
        let r = bundled();
        assert!(!r.rules.is_empty());
        let t = |n: &str| tier_with(&r, n, true);
        assert_eq!(t("openrouter-qwen-free"), CostTier::Free);
        assert_eq!(t("anthropic-claude-opus-4"), CostTier::High);
        assert_eq!(t("openai-gpt-4o-mini"), CostTier::Low);
        assert_eq!(t("googlegemini-gemini-3.6-flash"), CostTier::Low);
        assert_eq!(t("groq-openai-gpt-oss-20b"), CostTier::Low);
        assert_eq!(
            t("anthropic-claude-sonnet-4"),
            CostTier::Mid,
            "unknown cloud is mid"
        );
    }

    #[test]
    fn local_engines_are_always_free() {
        assert_eq!(
            tier_with(&bundled(), "ollama-opus-lookalike", false),
            CostTier::Free
        );
    }

    #[test]
    fn first_matching_rule_wins() {
        let r = bundled();
        // `free` precedes `opus`.
        assert_eq!(tier_with(&r, "x-opus-free", true), CostTier::Free);
    }

    #[test]
    fn cost_matters_most_for_reflex_and_least_for_reasoning() {
        let high = CostTier::High;
        let b = Budget::Balanced;
        let r = penalty(high, TaskClass::Reflex, b);
        let c = penalty(high, TaskClass::Chat, b);
        let k = penalty(high, TaskClass::Code, b);
        let n = penalty(high, TaskClass::Reasoning, b);
        assert!(r > c && c > k && k > n && n > 0.0);
    }

    #[test]
    fn budget_dial_scales_the_penalty() {
        let p = |b| penalty(CostTier::Mid, TaskClass::Chat, b);
        assert_eq!(p(Budget::Max), 0.0);
        assert!((p(Budget::Low) - 2.0 * p(Budget::Balanced)).abs() < 1e-6);
        assert_eq!(penalty(CostTier::Free, TaskClass::Reflex, Budget::Low), 0.0);
    }

    #[test]
    fn budget_parses_common_spellings_and_rejects_junk() {
        assert_eq!(Budget::parse("LOW"), Some(Budget::Low));
        assert_eq!(Budget::parse("max"), Some(Budget::Max));
        assert_eq!(Budget::parse(""), Some(Budget::Balanced));
        assert_eq!(Budget::parse("cheapest!!"), None);
    }
}
