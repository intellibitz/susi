//! Multi-provider consensus for high-risk missions — unweighted majority and
//! Dawid-Skene Bayesian log-odds weighted consensus with semantic equivalence clustering.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusResult {
    pub agreed: bool,
    pub value: String,
}

/// Require a majority agreement across provider answers.
#[must_use]
pub fn consensus(answers: &[String]) -> ConsensusResult {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for a in answers {
        *counts.entry(a.as_str()).or_default() += 1;
    }
    let Some((value, n)) = counts.into_iter().max_by_key(|(_, n)| *n) else {
        return ConsensusResult {
            agreed: false,
            value: String::new(),
        };
    };
    ConsensusResult {
        agreed: n * 2 > answers.len(),
        value: value.into(),
    }
}

/// One provider's vote with its empirical Beta-smoothed reliability $p_i \in (0, 1)$.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedVote {
    pub provider: String,
    pub answer: String,
    /// Empirical reliability $p_i \in (0, 1)$ from `susi-gemi::engines::brain`.
    pub reliability: f64,
}

/// Full Bayesian Dawid-Skene consensus diagnosis across multiple providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BayesianConsensusReport {
    pub agreed: bool,
    /// Canonical winning answer text.
    pub value: String,
    /// Exact Bayesian posterior probability $P(Y = \hat{y} \mid \text{votes}) \in [0, 1]$.
    pub posterior_probability: f64,
    /// Shannon entropy $H(Y \mid \text{votes})$ in bits across equivalence clusters.
    pub shannon_entropy_bits: f64,
    /// Log-odds weighted support fraction of the winning cluster.
    pub weighted_support_fraction: f64,
    /// Number of distinct semantic equivalence clusters observed.
    pub cluster_count: usize,
    /// Winning providers that voted for the consensus cluster.
    pub supporting_providers: Vec<String>,
}

/// Normalize an answer into its canonical semantic equivalence key (stripping
/// markdown code fences, collapsing whitespace runs, trimming punctuation, and
/// case-folding) so superficial formatting differences coalesce before voting.
#[must_use]
pub fn canonicalize_answer(raw: &str) -> String {
    let trimmed = raw.trim();
    let unfenced = if trimmed.starts_with("```") {
        let inner = trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```rust")
            .trim_start_matches("```");
        inner.trim_end_matches("```").trim()
    } else {
        trimmed
    };
    let collapsed = unfenced
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    collapsed
        .trim_end_matches(['.', ';', ','])
        .trim()
        .trim_matches(['*', '_', '`'])
        .trim()
        .to_string()
}

/// Compute Dawid-Skene Bayesian log-odds weighted consensus across provider votes.
///
/// Under an independent confusion model over $K = \max(2, |\mathcal{C}|)$ semantic
/// clusters where provider $i$ emits the true cluster with probability $p_i$ and
/// any false cluster with probability $\frac{1 - p_i}{K - 1}$, the log-likelihood of
/// cluster $c$ is:
/// $$\ell(c) = \sum_{i \in c} \ln p_i + \sum_{i \notin c} \ln \frac{1 - p_i}{K - 1}$$
#[must_use]
pub fn bayesian_consensus(
    votes: &[WeightedVote],
    min_posterior_confidence: f64,
) -> BayesianConsensusReport {
    if votes.is_empty() {
        return BayesianConsensusReport {
            agreed: false,
            value: String::new(),
            posterior_probability: 0.0,
            shannon_entropy_bits: 0.0,
            weighted_support_fraction: 0.0,
            cluster_count: 0,
            supporting_providers: Vec::new(),
        };
    }

    // Group votes by canonical semantic key while keeping the highest-reliability
    // original answer string as the cluster's representative text.
    struct Cluster {
        representative: String,
        best_rel: f64,
        voters: Vec<(String, f64)>,
        log_odds_weight: f64,
    }
    let mut clusters: BTreeMap<String, Cluster> = BTreeMap::new();
    for v in votes {
        let key = canonicalize_answer(&v.answer);
        let p = v.reliability.clamp(0.05, 0.995);
        // Log-odds weight shifted so p >= 0.05 maps to a positive reliability weight:
        let weight = (p / (1.0 - p)).ln() - (0.05_f64 / 0.95_f64).ln();
        let entry = clusters.entry(key).or_insert_with(|| Cluster {
            representative: v.answer.clone(),
            best_rel: p,
            voters: Vec::new(),
            log_odds_weight: 0.0,
        });
        if p > entry.best_rel {
            entry.best_rel = p;
            entry.representative = v.answer.clone();
        }
        entry.voters.push((v.provider.clone(), p));
        entry.log_odds_weight += weight;
    }

    let k_outcomes = (clusters.len().max(2)) as f64;
    let mut log_posteriors: BTreeMap<String, f64> = BTreeMap::new();
    for key in clusters.keys() {
        let mut ll = 0.0_f64;
        for v in votes {
            let v_key = canonicalize_answer(&v.answer);
            let p = v.reliability.clamp(0.05, 0.995);
            if &v_key == key {
                ll += p.ln();
            } else {
                ll += ((1.0 - p) / (k_outcomes - 1.0)).ln();
            }
        }
        log_posteriors.insert(key.clone(), ll);
    }

    let max_ll = log_posteriors
        .values()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let sum_exp: f64 = log_posteriors.values().map(|&v| (v - max_ll).exp()).sum();

    let mut posteriors: BTreeMap<String, f64> = BTreeMap::new();
    for (k, &ll) in &log_posteriors {
        posteriors.insert(k.clone(), ((ll - max_ll).exp() / sum_exp).clamp(0.0, 1.0));
    }

    let total_weight: f64 = clusters.values().map(|c| c.log_odds_weight).sum();
    let Some((winner_key, &posterior_probability)) = posteriors
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
    else {
        return BayesianConsensusReport {
            agreed: false,
            value: String::new(),
            posterior_probability: 0.0,
            shannon_entropy_bits: 0.0,
            weighted_support_fraction: 0.0,
            cluster_count: 0,
            supporting_providers: Vec::new(),
        };
    };

    let shannon_entropy_bits: f64 = posteriors
        .values()
        .filter(|&&p| p > 1e-12)
        .map(|&p| -p * p.log2())
        .sum::<f64>()
        .max(0.0);

    let Some(winner) = clusters.get(winner_key) else {
        return BayesianConsensusReport {
            agreed: false,
            value: String::new(),
            posterior_probability: 0.0,
            shannon_entropy_bits: 0.0,
            weighted_support_fraction: 0.0,
            cluster_count: 0,
            supporting_providers: Vec::new(),
        };
    };
    let weighted_support_fraction = if total_weight > 0.0 {
        (winner.log_odds_weight / total_weight).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let agreed = posterior_probability >= min_posterior_confidence.clamp(0.5, 0.999)
        && weighted_support_fraction > 0.5;

    BayesianConsensusReport {
        agreed,
        value: winner.representative.clone(),
        posterior_probability,
        shannon_entropy_bits,
        weighted_support_fraction,
        cluster_count: clusters.len(),
        supporting_providers: winner.voters.iter().map(|(p, _)| p.clone()).collect(),
    }
}

#[cfg(test)]
mod consensus_mode_tests {
    use super::*;

    #[test]
    fn consensus_mode_requires_majority() {
        let r = consensus(&["a".into(), "a".into(), "b".into()]);
        assert!(r.agreed);
        assert_eq!(r.value, "a");
        assert!(!consensus(&["a".into(), "b".into()]).agreed);
    }

    #[test]
    fn bayesian_consensus_coalesces_equivalent_formats_and_weights_reliability() {
        let votes = vec![
            WeightedVote {
                provider: "anthropic-opus".into(),
                answer: "```json\n{\"status\": \"ok\"}\n```".into(),
                reliability: 0.96,
            },
            WeightedVote {
                provider: "deepseek-r1".into(),
                answer: "{\"status\": \"ok\"}.".into(),
                reliability: 0.92,
            },
            WeightedVote {
                provider: "untested-small".into(),
                answer: "{\"status\": \"fail\"}".into(),
                reliability: 0.45,
            },
        ];
        let report = bayesian_consensus(&votes, 0.85);
        assert!(report.agreed);
        assert_eq!(report.cluster_count, 2);
        assert!(report.posterior_probability > 0.95);
        assert_eq!(
            report.supporting_providers,
            vec!["anthropic-opus".to_string(), "deepseek-r1".to_string()]
        );
    }

    #[test]
    fn consensus_empty_slice_returns_not_agreed_and_empty_value() {
        let empty = consensus(&[]);
        assert!(!empty.agreed);
        assert!(empty.value.is_empty());
    }

    #[test]
    fn bayesian_consensus_empty_votes_returns_zero_clusters_and_probability() {
        let report = bayesian_consensus(&[], 0.85);
        assert!(!report.agreed);
        assert!(report.value.is_empty());
        assert_eq!(report.cluster_count, 0);
        assert!(report.posterior_probability.abs() < f64::EPSILON);
        assert!(report.supporting_providers.is_empty());
    }

    #[test]
    fn canonicalize_answer_strips_code_fences_punctuation_and_markdown_emphasis() {
        let fenced = "```rust\nfn main() {}\n```";
        assert_eq!(canonicalize_answer(fenced), "fn main() {}");
        assert_eq!(canonicalize_answer("  **Yes, agreed!**.  "), "yes, agreed!");
    }
}
