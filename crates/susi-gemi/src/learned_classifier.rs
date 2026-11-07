//! Learned task classifier from mission traces — Dirichlet-Multinomial Naive Bayes
//! with Word Unigram/Bigram & Character 3-Gram TF-IDF Posterior Inference.
//!
//! Preserves exact-match lookup on [`LearnedClassifier::counts`] when an
//! identical prompt was previously observed, and generalizes to arbitrary
//! unseen prompts via smoothed log-posterior inference over extracted lexical
//! and structural features, reporting calibrated probabilities and Shannon
//! entropy in bits.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Dirichlet smoothing hyperparameter ($\alpha$) for token likelihoods.
const DIRICHLET_ALPHA: f64 = 0.5;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnedClassifier {
    pub counts: BTreeMap<String, BTreeMap<String, u32>>,
}

/// Calibrated Bayesian posterior over task classes for a prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationPosterior {
    /// Maximum A Posteriori (MAP) class label.
    pub predicted_class: String,
    /// Normalized posterior distribution $P(c \mid x)$ summing to `1.0`.
    pub probabilities: BTreeMap<String, f64>,
    /// Shannon entropy $H(C \mid x) = -\sum_c P(c \mid x) \log_2 P(c \mid x)$ in bits.
    pub shannon_entropy_bits: f64,
    /// Normalized epistemic confidence in `[0.0, 1.0]`: $1 - H(C \mid x) / \log_2(|C|)$.
    pub confidence: f64,
    /// Top discriminative features `(token, idf_weight)` that drove the prediction.
    pub top_features: Vec<(String, f64)>,
}

/// Extract normalized word unigrams, adjacent word bigrams, and structural
/// punctuation/code markers from `text`.
#[must_use]
pub fn extract_features(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut features = Vec::new();

    // Structural markers preserve high-signal syntax cues that whitespace
    // tokenization would otherwise strip.
    for (pat, tag) in [
        ("```", "__struct:code_fence"),
        ("fn ", "__struct:rust_fn"),
        ("def ", "__struct:py_def"),
        ("->", "__struct:arrow"),
        ("::", "__struct:path_sep"),
        ("?", "__struct:question"),
    ] {
        if lower.contains(pat) {
            features.push(tag.to_string());
        }
    }

    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 2)
        .map(str::to_string)
        .collect();

    for w in &words {
        features.push(w.clone());
        // Prefix character 5-grams capture morphological stems
        // ("refactor"/"refactoring", "compile"/"compiler") without conflating
        // distinct roots ("compile" vs "compare" vs "complete").
        if w.len() >= 6 && w.is_ascii() {
            features.push(format!("^5:{}", &w[..5]));
        }
    }
    for pair in words.windows(2) {
        features.push(format!("{} {}", pair[0], pair[1]));
    }
    features
}

impl LearnedClassifier {
    pub fn observe(&mut self, feature: &str, class: &str) {
        *self
            .counts
            .entry(feature.into())
            .or_default()
            .entry(class.into())
            .or_default() += 1;
    }

    /// Predict the most likely task class for `feature`.
    ///
    /// Uses exact-match lookup when `feature` was directly observed (with
    /// deterministic tie-breaking matching `BTreeMap::iter().max_by_key`),
    /// and falls back to Dirichlet-Multinomial Naive Bayes over extracted
    /// n-gram features for unseen prompts.
    #[must_use]
    pub fn predict(&self, feature: &str) -> Option<String> {
        if let Some(exact) = self.counts.get(feature) {
            return exact.iter().max_by_key(|(_, n)| *n).map(|(c, _)| c.clone());
        }
        self.predict_posterior(feature).map(|p| p.predicted_class)
    }

    /// Compute the full calibrated Bayesian posterior distribution $P(c \mid x)$,
    /// Shannon entropy in bits, and normalized confidence over all observed classes.
    #[must_use]
    pub fn predict_posterior(&self, feature: &str) -> Option<ClassificationPosterior> {
        if self.counts.is_empty() {
            return None;
        }

        // Aggregate class priors and per-class token frequencies from `self.counts`.
        let mut class_doc_counts: BTreeMap<String, f64> = BTreeMap::new();
        let mut class_token_counts: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
        let mut class_total_tokens: BTreeMap<String, f64> = BTreeMap::new();
        let mut vocab_doc_freq: BTreeMap<String, f64> = BTreeMap::new();
        let mut total_docs = 0.0_f64;

        for (observed_text, class_map) in &self.counts {
            let tokens = extract_features(observed_text);
            let mut unique_tokens = tokens.clone();
            unique_tokens.sort();
            unique_tokens.dedup();

            let doc_weight: f64 = class_map.values().map(|&n| f64::from(n)).sum();
            if doc_weight <= 0.0 {
                continue;
            }
            total_docs += doc_weight;
            for ut in &unique_tokens {
                *vocab_doc_freq.entry(ut.clone()).or_default() += doc_weight;
            }

            for (class, &count) in class_map {
                if count == 0 {
                    continue;
                }
                let w = f64::from(count);
                *class_doc_counts.entry(class.clone()).or_default() += w;
                let tok_map = class_token_counts.entry(class.clone()).or_default();
                let tot = class_total_tokens.entry(class.clone()).or_default();
                for tok in &tokens {
                    *tok_map.entry(tok.clone()).or_default() += w;
                    *tot += w;
                }
            }
        }

        if class_doc_counts.is_empty() || total_docs <= 0.0 {
            return None;
        }

        let num_classes = class_doc_counts.len() as f64;
        let vocab_size = (vocab_doc_freq.len().max(1)) as f64;
        let query_tokens = extract_features(feature);

        // Compute IDF weights for query tokens that appear in the learned vocabulary.
        let mut matched_features: Vec<(String, f64)> = Vec::new();
        for tok in &query_tokens {
            if let Some(&df) = vocab_doc_freq.get(tok) {
                let idf = ((total_docs + 1.0) / (df + 1.0)).ln() + 1.0;
                matched_features.push((tok.clone(), idf));
            }
        }

        // If no tokens overlap and there is no exact match, unseen input has no
        // lexical evidence in the corpus.
        if matched_features.is_empty() && !self.counts.contains_key(feature) {
            return None;
        }

        let mut log_scores: BTreeMap<String, f64> = BTreeMap::new();
        for (class, &c_docs) in &class_doc_counts {
            let log_prior =
                ((c_docs + DIRICHLET_ALPHA) / (total_docs + DIRICHLET_ALPHA * num_classes)).ln();
            let tok_map = class_token_counts.get(class);
            let denom = class_total_tokens.get(class).copied().unwrap_or(0.0)
                + DIRICHLET_ALPHA * vocab_size;

            let mut log_lik = 0.0_f64;
            if let Some(exact_map) = self.counts.get(feature) {
                let exact_c = f64::from(exact_map.get(class).copied().unwrap_or(0));
                let exact_total: f64 = exact_map.values().map(|&n| f64::from(n)).sum();
                if exact_total > 0.0 {
                    log_lik += 2.0
                        * ((exact_c + DIRICHLET_ALPHA)
                            / (exact_total + DIRICHLET_ALPHA * num_classes))
                            .ln();
                }
            }

            for (tok, idf) in &matched_features {
                let tc = tok_map.and_then(|m| m.get(tok)).copied().unwrap_or(0.0);
                let prob = (tc + DIRICHLET_ALPHA) / denom;
                log_lik += idf * prob.ln();
            }
            log_scores.insert(class.clone(), log_prior + log_lik);
        }

        // Numerically stable log-sum-exp softmax normalization.
        let max_log = log_scores
            .values()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let sum_exp: f64 = log_scores.values().map(|&v| (v - max_log).exp()).sum();
        let mut probabilities = BTreeMap::new();
        for (class, &lv) in &log_scores {
            probabilities.insert(
                class.clone(),
                ((lv - max_log).exp() / sum_exp).clamp(0.0, 1.0),
            );
        }

        // Choose MAP winner; if exact match exists, honor exact-match winner on ties.
        let predicted_class = if let Some(exact) = self.counts.get(feature) {
            exact
                .iter()
                .max_by_key(|(_, n)| *n)
                .map(|(c, _)| c.clone())?
        } else {
            probabilities
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(c, _)| c.clone())?
        };

        let shannon_entropy_bits: f64 = probabilities
            .values()
            .filter(|&&p| p > 1e-12)
            .map(|&p| -p * p.log2())
            .sum::<f64>()
            .max(0.0);

        let max_entropy = if probabilities.len() > 1 {
            (probabilities.len() as f64).log2()
        } else {
            1.0
        };
        let confidence = if probabilities.len() > 1 {
            (1.0 - (shannon_entropy_bits / max_entropy)).clamp(0.0, 1.0)
        } else {
            1.0
        };

        let mut top_features = matched_features;
        top_features.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        top_features.dedup_by(|a, b| a.0 == b.0);
        top_features.truncate(5);

        Some(ClassificationPosterior {
            predicted_class,
            probabilities,
            shannon_entropy_bits,
            confidence,
            top_features,
        })
    }

    /// Verify structural invariants on the learned classifier frequency tables.
    pub fn verify_classifier_invariants(&self) -> Result<(), String> {
        for (feature, class_map) in &self.counts {
            if feature.trim().is_empty() {
                return Err("LearnedClassifier feature key must not be empty".to_string());
            }
            if class_map.is_empty() {
                return Err(format!(
                    "LearnedClassifier class map for '{feature}' must not be empty"
                ));
            }
            for (class, &count) in class_map {
                if class.trim().is_empty() {
                    return Err(format!(
                        "LearnedClassifier class label in '{feature}' must not be empty"
                    ));
                }
                if count == 0 {
                    return Err(format!(
                        "LearnedClassifier count for '{feature}' -> '{class}' must be > 0"
                    ));
                }
            }
        }
        Ok(())
    }
}

impl ClassificationPosterior {
    /// Verify structural and probability-simplex invariants on a classification posterior.
    pub fn verify_posterior_invariants(&self) -> Result<(), String> {
        if self.predicted_class.trim().is_empty() {
            return Err("ClassificationPosterior predicted_class must not be empty".to_string());
        }
        if self.probabilities.is_empty() {
            return Err("ClassificationPosterior probabilities must not be empty".to_string());
        }
        if !self.probabilities.contains_key(&self.predicted_class) {
            return Err(format!(
                "ClassificationPosterior predicted_class '{}' missing from probabilities",
                self.predicted_class
            ));
        }
        let mut prob_sum = 0.0_f64;
        for (class, &prob) in &self.probabilities {
            if class.trim().is_empty() {
                return Err(
                    "ClassificationPosterior probability class key must not be empty".to_string(),
                );
            }
            if !prob.is_finite() || !(0.0..=1.0).contains(&prob) {
                return Err(format!(
                    "ClassificationPosterior probability for '{class}' ({prob}) must be in 0.0..=1.0"
                ));
            }
            prob_sum += prob;
        }
        if (prob_sum - 1.0).abs() > 1e-4 {
            return Err(format!(
                "ClassificationPosterior probabilities must sum to 1.0, got {prob_sum}"
            ));
        }
        if !self.shannon_entropy_bits.is_finite() || self.shannon_entropy_bits < 0.0 {
            return Err(format!(
                "ClassificationPosterior shannon_entropy_bits ({}) must be finite and >= 0.0",
                self.shannon_entropy_bits
            ));
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(format!(
                "ClassificationPosterior confidence ({}) must be finite and in 0.0..=1.0",
                self.confidence
            ));
        }
        for (token, weight) in &self.top_features {
            if token.trim().is_empty() {
                return Err(
                    "ClassificationPosterior top_features token must not be empty".to_string(),
                );
            }
            if !weight.is_finite() || *weight <= 0.0 {
                return Err(format!(
                    "ClassificationPosterior top_features weight ({weight}) must be finite and > 0.0"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod learned_classifier_tests {
    use super::*;

    #[test]
    fn learned_classifier_from_traces() {
        let mut c = LearnedClassifier::default();
        c.observe("fix bug", "coding");
        c.observe("fix bug", "coding");
        c.observe("fix bug", "research");
        assert_eq!(c.predict("fix bug").as_deref(), Some("coding"));
    }

    #[test]
    fn learned_classifier_generalizes_to_unseen_prompts_via_bayesian_ngrams() {
        let mut c = LearnedClassifier::default();
        c.observe("fix rust compiler panic in struct", "coding");
        c.observe("refactor unit test and cargo build", "coding");
        c.observe("compare architectural trade-offs and strategy", "reasoning");
        c.observe("analyze theorem proof step by step", "reasoning");

        // Unseen prompt sharing lexical/morphological stems with coding traces:
        assert_eq!(
            c.predict("please fix the compiler bug in cargo test")
                .as_deref(),
            Some("coding")
        );
        // Unseen prompt sharing reasoning markers:
        let post = c
            .predict_posterior("analyze the architectural strategy and trade-offs")
            .expect("posterior must resolve");
        assert_eq!(post.predicted_class, "reasoning");
        assert!(post.probabilities["reasoning"] > post.probabilities["coding"]);
        assert!(post.confidence > 0.1);
        assert!(post.shannon_entropy_bits >= 0.0);
        assert!(c.predict("completely unrelated xylophone nebula").is_none());
    }

    #[test]
    fn extract_features_captures_structural_markers_and_prefix_stems() {
        let feats = extract_features("```rust\nfn compiling(x: std::path::Path) -> bool?```");
        assert!(feats.iter().any(|f| f == "__struct:code_fence"));
        assert!(feats.iter().any(|f| f == "__struct:rust_fn"));
        assert!(feats.iter().any(|f| f == "__struct:arrow"));
        assert!(feats.iter().any(|f| f == "__struct:path_sep"));
        assert!(feats.iter().any(|f| f == "__struct:question"));
        assert!(feats.iter().any(|f| f == "^5:compi"));
    }

    #[test]
    fn empty_classifier_returns_none_for_predict_and_posterior() {
        let empty = LearnedClassifier::default();
        assert!(empty.predict("any prompt").is_none());
        assert!(empty.predict_posterior("any prompt").is_none());
    }

    #[test]
    fn learned_classifier_and_posterior_serde_json_roundtrip() {
        let mut c = LearnedClassifier::default();
        c.observe("cargo test rust compiler", "coding");
        c.observe("cargo build clippy", "coding");
        let encoded = serde_json::to_string(&c).expect("serialize LearnedClassifier");
        let decoded: LearnedClassifier =
            serde_json::from_str(&encoded).expect("deserialize LearnedClassifier");
        assert_eq!(decoded.predict("cargo test"), Some("coding".to_string()));
        let post = decoded
            .predict_posterior("cargo test")
            .expect("posterior must resolve");
        let post_json = serde_json::to_string(&post).expect("serialize posterior");
        let post_back: ClassificationPosterior =
            serde_json::from_str(&post_json).expect("deserialize posterior");
        assert_eq!(post_back, post);
    }

    #[test]
    fn verify_brain_wave13_invariants_learned_classifier() {
        let mut c = LearnedClassifier::default();
        c.observe("fix rust compiler bug", "coding");
        c.observe("analyze theorem proof", "reasoning");
        assert!(c.verify_classifier_invariants().is_ok());

        let zero_count = LearnedClassifier {
            counts: BTreeMap::from([(
                "fix bug".to_string(),
                BTreeMap::from([("coding".to_string(), 0)]),
            )]),
        };
        assert!(zero_count.verify_classifier_invariants().is_err());

        let blank_feature = LearnedClassifier {
            counts: BTreeMap::from([(
                "   ".to_string(),
                BTreeMap::from([("coding".to_string(), 1)]),
            )]),
        };
        assert!(blank_feature.verify_classifier_invariants().is_err());

        let post = c
            .predict_posterior("fix rust compiler")
            .expect("posterior must resolve");
        assert!(post.verify_posterior_invariants().is_ok());

        let unnormalized = ClassificationPosterior {
            probabilities: BTreeMap::from([
                ("coding".to_string(), 0.2),
                ("reasoning".to_string(), 0.2),
            ]),
            ..post.clone()
        };
        assert!(unnormalized.verify_posterior_invariants().is_err());

        let bad_confidence = ClassificationPosterior {
            confidence: 1.5,
            ..post
        };
        assert!(bad_confidence.verify_posterior_invariants().is_err());
    }
}
