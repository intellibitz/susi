//! Test for provider ranking by marginal cost and quota scarcity (VC-202-021, T-DEEPSEEK-120).
//! Verifies that ranking prefers providers with lower marginal cost and abundant quota,
//! degrading gracefully toward those with higher cost or scarce capacity.
//!
//! This test exercises:
//! - Ranking multiple providers by expected marginal cost of next call
//! - Scarcity adjustment: cost increases as quota approaches exhaustion
//! - Preference ordering: abundant quota > scarce quota > exhausted
//! - Cost-aware selection avoiding price-per-token naïveté

/// Provider ranking score for one call: marginal cost + scarcity penalty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingScore {
    /// Expected marginal cost of the next call in USD.
    pub marginal_cost: f64,
    /// Scarcity penalty (0.0 = abundant, 1.0+ = exhausted/unavailable).
    pub scarcity_penalty: f64,
}

impl RankingScore {
    /// Create a ranking score from marginal cost and scarcity.
    #[must_use]
    pub fn new(marginal_cost: f64, scarcity_penalty: f64) -> Self {
        Self {
            marginal_cost,
            scarcity_penalty,
        }
    }

    /// Total effective cost: marginal cost + scarcity penalty.
    /// Scarcity penalty is multiplicative to cost, not additive.
    #[must_use]
    pub fn effective_cost(&self) -> f64 {
        self.marginal_cost * (1.0 + self.scarcity_penalty)
    }
}

impl Ord for RankingScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.effective_cost()
            .partial_cmp(&other.effective_cost())
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl Eq for RankingScore {}

impl PartialOrd for RankingScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Provider entry for ranking: name and its ranking score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedProvider {
    pub name: String,
    pub score: RankingScore,
}

impl PartialOrd for RankedProvider {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedProvider {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score.cmp(&other.score)
    }
}

/// Rank providers by marginal cost and quota scarcity.
/// Returns providers sorted from best (lowest cost) to worst (highest cost or unavailable).
#[must_use]
pub fn rank_providers(providers: &[(String, RankingScore)]) -> Vec<RankedProvider> {
    let mut ranked: Vec<RankedProvider> = providers
        .iter()
        .map(|(name, score)| RankedProvider {
            name: name.clone(),
            score: *score,
        })
        .collect();
    ranked.sort();
    ranked
}

#[test]
fn ranking_by_marginal_cost() {
    // Provider A: pay-as-you-go, $0.001/token, abundant quota
    let provider_a = ("openai".to_string(), RankingScore::new(0.001, 0.0));

    // Provider B: pay-as-you-go, $0.002/token, abundant quota
    let provider_b = ("anthropic".to_string(), RankingScore::new(0.002, 0.0));

    // Provider C: pay-as-you-go, $0.001/token, but quota nearly exhausted (scarcity 0.5)
    let provider_c = ("groq".to_string(), RankingScore::new(0.001, 0.5));

    // Provider D: pay-as-you-go, $0.002/token, quota exhausted
    let provider_d = (
        "mistral".to_string(),
        RankingScore::new(0.002, f64::INFINITY),
    );

    let providers = vec![provider_d, provider_c, provider_b, provider_a];
    let ranked = rank_providers(&providers);

    // Expected ranking (best to worst):
    // 1. openai: 0.001 * (1 + 0) = 0.001
    // 2. groq: 0.001 * (1 + 0.5) = 0.0015 (same base cost but scarce)
    // 3. anthropic: 0.002 * (1 + 0) = 0.002 (higher base cost)
    // 4. mistral: 0.002 * INFINITY = INFINITY (exhausted)

    assert_eq!(ranked[0].name, "openai");
    assert_eq!(ranked[1].name, "groq"); // scarce but still cheaper than anthropic
    assert_eq!(ranked[2].name, "anthropic");
    assert_eq!(ranked[3].name, "mistral"); // exhausted - last choice

    // Verify effective costs
    assert!((ranked[0].score.effective_cost() - 0.001).abs() < 0.0001);
    assert!((ranked[1].score.effective_cost() - 0.0015).abs() < 0.0001);
    assert!((ranked[2].score.effective_cost() - 0.002).abs() < 0.0001);
    assert_eq!(ranked[3].score.effective_cost(), f64::INFINITY);

    // Verify sorting: each provider is >= the previous one
    for i in 1..ranked.len() {
        assert!(
            ranked[i].score.effective_cost() >= ranked[i - 1].score.effective_cost(),
            "Ranking not sorted: {} >= {}",
            ranked[i].score.effective_cost(),
            ranked[i - 1].score.effective_cost()
        );
    }

    // Summary: Ranking by marginal cost + scarcity penalty enables scheduling to:
    // - Prefer providers with lower cost per call
    // - Degrade gracefully as quota shrinks (scarcity raises cost)
    // - Block use of exhausted endpoints (infinite cost)
    // This replaces naive price-per-token ranking with cost-aware scheduling.
}
