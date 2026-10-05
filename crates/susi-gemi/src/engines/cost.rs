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

/// Canonical token profile of one `class` task — prompt tokens, completion
/// tokens, and the prompt-cache hit ratio the workload is expected to
/// achieve (VC-202-002). Repeat-context classes carry a long shared prefix
/// (system prompt, workspace context, tool schemas) that vendor prompt
/// caching bills at the cheaper hit rate; providers that report no cache
/// counters cannot be measured, so these are declared workload profiles,
/// not telemetry.
pub fn task_token_profile(class: super::brain::TaskClass) -> (u64, u64, f64) {
    use super::brain::TaskClass;
    match class {
        TaskClass::Reflex => (512, 128, 0.0),
        TaskClass::Chat => (2_048, 384, 0.25),
        TaskClass::Code => (16_384, 1_536, 0.6),
        TaskClass::Reasoning => (49_152, 3_072, 0.75),
    }
}

/// The installed price catalog, loaded from `model_prices.json` in the
/// config dir — the file a signed `model-prices` channel bundle lands as.
/// Re-read per call so a fresh catalog is picked up without a restart; an
/// absent or unparsable file prices nothing rather than guessing.
/// `SUSI_PRICE_CATALOG_FILE` overrides the path (tests point it at their
/// own file — the host's real catalog is never read under `cfg(test)`).
pub(crate) fn price_catalog() -> Option<crate::models::price_catalog::PriceCatalog> {
    let path =
        if let Some(p) = std::env::var_os("SUSI_PRICE_CATALOG_FILE").filter(|p| !p.is_empty()) {
            std::path::PathBuf::from(p)
        } else {
            if cfg!(test) {
                return None;
            }
            susi_paths::SusiDirs::config_dir().join("model_prices.json")
        };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| crate::models::price_catalog::PriceCatalog::from_json(&s).ok())
}

/// Provider name → catalog entry: exact match first, then the longest
/// catalog `model_id` contained in the composite provider name (the same
/// contains-rule style `tier_of` uses for `cost_tiers.json`).
pub(crate) fn catalog_entry_for<'a>(
    catalog: &'a crate::models::price_catalog::PriceCatalog,
    provider: &str,
) -> Option<&'a crate::models::price_catalog::PriceEntry> {
    if let Some(entry) = catalog.lookup(provider) {
        return Some(entry);
    }
    let name = provider.to_ascii_lowercase();
    catalog
        .entries
        .values()
        .filter(|entry| name.contains(&entry.model_id.to_ascii_lowercase()))
        .max_by_key(|entry| entry.model_id.len())
}

/// Expected USD for one call: `prompt`/`completion` token counts and the
/// cache hit ratio the workload actually achieves, priced against the
/// catalog's cache-hit/miss rates (VC-202-002). `None` when the provider
/// has no price record — callers keep the coarse-tier fallback.
pub fn expected_call_cost_usd_in(
    catalog: Option<&crate::models::price_catalog::PriceCatalog>,
    provider: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_hit_ratio: f64,
) -> Option<f64> {
    let catalog = catalog?;
    let entry = catalog_entry_for(catalog, provider)?;
    catalog.expected_cost_usd(
        &entry.model_id,
        prompt_tokens,
        completion_tokens,
        cache_hit_ratio,
    )
}

/// As [`expected_call_cost_usd_in`], priced against the installed catalog.
pub fn expected_call_cost_usd(
    provider: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_hit_ratio: f64,
) -> Option<f64> {
    expected_call_cost_usd_in(
        price_catalog().as_ref(),
        provider,
        prompt_tokens,
        completion_tokens,
        cache_hit_ratio,
    )
}

/// Expected USD for one whole `class` task on `provider` — the canonical
/// [`task_token_profile`] priced against the catalog, cache-hit rate
/// included. `None` when no price record exists (VC-202-002).
pub fn expected_task_cost_usd_in(
    catalog: Option<&crate::models::price_catalog::PriceCatalog>,
    provider: &str,
    class: super::brain::TaskClass,
) -> Option<f64> {
    let (prompt, completion, hit_ratio) = task_token_profile(class);
    expected_call_cost_usd_in(catalog, provider, prompt, completion, hit_ratio)
}

/// As [`expected_task_cost_usd_in`], priced against the installed catalog.
pub fn expected_task_cost_usd(provider: &str, class: super::brain::TaskClass) -> Option<f64> {
    expected_task_cost_usd_in(price_catalog().as_ref(), provider, class)
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
