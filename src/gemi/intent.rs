use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IntentCategory {
    Coding,
    Mathematics,
    Reasoning,
    Creative,
    #[default]
    General,
}

/// How demanding a prompt likely is, on a 5-level scale. Orthogonal to
/// `IntentCategory` (domain vs. difficulty) - a one-line arithmetic
/// question and a one-line "write a poem" prompt are both trivial even
/// though they'd classify into different `IntentCategory` values.
///
/// Deliberately a *percentile* concept (`target_percentile`), not a fixed
/// GB threshold: which actual model that maps to depends entirely on
/// what's resident (2 tiers today - baseline + best-fit; potentially more
/// once the download policy fetches middle tiers too), not a hardcoded
/// size. Mandate 35 (100% Dynamic Config): the classifier expresses a
/// *position* between whatever's available, never a specific model size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum TaskComplexity {
    Trivial,
    Simple,
    Moderate,
    #[default]
    Complex,
    VeryComplex,
}

impl TaskComplexity {
    /// One level up, saturating at `VeryComplex`. Used to force genuine
    /// escalation on a retry after a verified failure (see
    /// `ModelManager::get_selected_model_for_request_with_min_complexity`),
    /// rather than relying on a longer retry prompt happening to cross a
    /// heuristic threshold on its own.
    pub fn escalate(self) -> Self {
        match self {
            TaskComplexity::Trivial => TaskComplexity::Simple,
            TaskComplexity::Simple => TaskComplexity::Moderate,
            TaskComplexity::Moderate => TaskComplexity::Complex,
            TaskComplexity::Complex => TaskComplexity::VeryComplex,
            TaskComplexity::VeryComplex => TaskComplexity::VeryComplex,
        }
    }

    /// Where this complexity level should sit between the smallest (0.0)
    /// and largest (1.0) resident model, by size. See
    /// `ModelManager::size_preference_score`, which scores candidates by
    /// closeness to this target rather than always preferring the
    /// biggest (or always the smallest).
    pub fn target_percentile(self) -> f32 {
        match self {
            TaskComplexity::Trivial => 0.0,
            TaskComplexity::Simple => 0.25,
            TaskComplexity::Moderate => 0.5,
            TaskComplexity::Complex => 0.75,
            TaskComplexity::VeryComplex => 1.0,
        }
    }
}

pub struct IntentClassifier;

impl IntentClassifier {
    /// Zero-latency heuristic for how demanding a prompt likely is, used
    /// to pick a *starting* tier instead of always defaulting to the
    /// biggest model available (see
    /// `ModelManager::identify_best_suited_local_model`'s `complexity`
    /// parameter). This is a starting-tier guess, not a correctness
    /// guarantee - there is no cheap way to know in advance whether a
    /// model can actually solve a given prompt, and precision beyond a
    /// handful of buckets from static text heuristics alone is inherently
    /// rough; a later escalate-on-failure mechanism (not yet built) is
    /// meant to correct a wrong initial guess, not this heuristic alone.
    /// Deliberately biased toward `Complex` (today's existing "prefer the
    /// biggest model" behavior) for anything that isn't a reasonably
    /// clear match to a lower bucket: misclassifying a simple prompt as
    /// complex just costs a bit more compute on a model that's already
    /// resident; the reverse risks a materially worse answer.
    pub fn classify_complexity(prompt: &str, context_words: Option<usize>) -> TaskComplexity {
        let trimmed = prompt.trim();
        let word_count = trimmed.split_whitespace().count();
        let lower = trimmed.to_lowercase();

        let multi_step_markers = [
            "step by step",
            "step-by-step",
            "first,",
            "then,",
            "after that",
            "and then",
            "multiple steps",
            "in detail",
        ];
        let has_multi_step_marker = multi_step_markers.iter().any(|m| lower.contains(m));
        let question_count = trimmed.matches('?').count();
        let sentence_count = trimmed.matches(['.', '!', '?']).count();
        let is_single_clause = question_count <= 1 && sentence_count <= 1;

        // VeryComplex: strong combined signal - explicit multi-step
        // language on an already-long prompt, or a long prompt with
        // several distinct questions.
        if word_count > 60 && (has_multi_step_marker || question_count > 1) {
            return TaskComplexity::VeryComplex;
        }
        // Complex (default bucket): any single strong signal - a
        // multi-step marker, more than one question, more than one
        // sentence, or simply a long prompt.
        if has_multi_step_marker || question_count > 1 || sentence_count > 2 || word_count > 60 {
            return TaskComplexity::Complex;
        }
        // Moderate: longer than a "simple" one-liner but no complexity
        // markers - a normal-length single-clause request.
        if word_count > 25 && is_single_clause {
            return TaskComplexity::Moderate;
        }
        // Simple: short, single-clause.
        if word_count > 4 && word_count <= 25 && is_single_clause {
            return TaskComplexity::Simple;
        }
        // Trivial: very short (greeting-length) and single-clause.
        if word_count <= 4 && is_single_clause {
            return TaskComplexity::Trivial;
        }

        TaskComplexity::Complex
    }

    /// Zero-latency heuristic intent classification based on semantic triggers.
    pub fn classify(prompt: &str) -> IntentCategory {
        let text = prompt.to_lowercase();

        let code_triggers = [
            "code", "impl", "rust", "python", "fn ", "def ", "class ", "function", "struct ",
            "script", "debug", "error:", "panic!", "compiler",
        ];
        let math_triggers = [
            "math",
            "equation",
            "calculus",
            "algebra",
            "integral",
            "derivative",
            "solve for",
            "theorem",
            "matrix",
        ];
        let logic_triggers = [
            "analyze",
            "reason",
            "evaluate",
            "why is",
            "compare",
            "contrast",
            "deduce",
            "logic",
            "explain how",
        ];
        let creative_triggers = [
            "write a poem",
            "story",
            "imagine",
            "creative",
            "generate an image",
            "roleplay",
            "write a song",
        ];

        let count_hits =
            |triggers: &[&str]| -> usize { triggers.iter().filter(|&&t| text.contains(t)).count() };

        let code_score = count_hits(&code_triggers);
        let math_score = count_hits(&math_triggers);
        let logic_score = count_hits(&logic_triggers);
        let creative_score = count_hits(&creative_triggers);

        let max_score = *[code_score, math_score, logic_score, creative_score, 0]
            .iter()
            .max()
            .unwrap_or(&0);

        if max_score == 0 {
            return IntentCategory::General;
        }

        if code_score == max_score && code_score > 0 {
            return IntentCategory::Coding;
        }
        if math_score == max_score && math_score > 0 {
            return IntentCategory::Mathematics;
        }
        if logic_score == max_score && logic_score > 0 {
            return IntentCategory::Reasoning;
        }
        if creative_score == max_score && creative_score > 0 {
            return IntentCategory::Creative;
        }

        IntentCategory::General
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_coding_prompt() {
        assert_eq!(
            IntentClassifier::classify("Please debug this rust fn that panics"),
            IntentCategory::Coding
        );
    }

    #[test]
    fn test_classify_math_prompt() {
        assert_eq!(
            IntentClassifier::classify("Solve for x in this algebra equation"),
            IntentCategory::Mathematics
        );
    }

    #[test]
    fn test_classify_reasoning_prompt() {
        assert_eq!(
            IntentClassifier::classify(
                "Analyze and compare these two theories, then explain how they differ"
            ),
            IntentCategory::Reasoning
        );
    }

    #[test]
    fn test_classify_creative_prompt() {
        assert_eq!(
            IntentClassifier::classify("write a poem about the ocean"),
            IntentCategory::Creative
        );
    }

    #[test]
    fn test_classify_general_prompt_with_no_triggers() {
        assert_eq!(
            IntentClassifier::classify("What time is it in Tokyo?"),
            IntentCategory::General
        );
    }

    #[test]
    fn test_classify_picks_highest_scoring_category_on_mixed_signals() {
        // Two code triggers ("rust", "fn ") vs one math trigger ("math") - coding should win.
        assert_eq!(
            IntentClassifier::classify("write a rust fn to check if a number is a math prime"),
            IntentCategory::Coding
        );
    }

    #[test]
    fn test_classify_complexity_greeting_is_trivial() {
        assert_eq!(
            IntentClassifier::classify_complexity("hi there", None),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn test_classify_complexity_short_single_question_is_simple() {
        assert_eq!(
            IntentClassifier::classify_complexity("What is the capital of France?", None),
            TaskComplexity::Simple
        );
    }

    #[test]
    fn test_classify_complexity_medium_single_clause_is_moderate() {
        // ~30 words, single sentence, no multi-step language - longer than
        // a quick factual ask but not clearly "complex" either.
        let prompt = "Describe the main differences between a hash map and a \
            balanced binary search tree in terms of average and worst-case \
            time complexity for insertion, lookup, and deletion operations.";
        assert_eq!(
            IntentClassifier::classify_complexity(prompt, None),
            TaskComplexity::Moderate
        );
    }

    #[test]
    fn test_classify_complexity_multi_step_marker_forces_complex_even_if_short() {
        assert_eq!(
            IntentClassifier::classify_complexity("explain this step by step", None),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_classify_complexity_multiple_questions_is_complex() {
        assert_eq!(
            IntentClassifier::classify_complexity("What is Rust? Why use it?", None),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_classify_complexity_long_multi_step_prompt_is_very_complex() {
        let prompt = "Design a distributed rate limiter that works correctly across \
            multiple independent servers without relying on a single point of \
            failure, handles clock skew between nodes gracefully, supports both \
            token-bucket and sliding-window strategies configurable per client, \
            persists state across restarts without losing accuracy, and degrades \
            predictably under network partitions. Walk through this step by step, \
            covering the data model, the synchronization protocol, and the failure \
            modes at each stage.";
        assert_eq!(
            IntentClassifier::classify_complexity(prompt, None),
            TaskComplexity::VeryComplex
        );
    }

    #[test]
    fn test_classify_complexity_defaults_to_complex_not_simple_when_ambiguous() {
        // Regression: complexity must be biased toward Complex (today's
        // existing "prefer the biggest model" behavior) for anything that
        // isn't clearly a lower bucket - a wrong "simple" guess risks a
        // materially worse answer, a wrong "complex" guess just costs a
        // bit more compute on an already-resident model.
        assert_eq!(TaskComplexity::default(), TaskComplexity::Complex);
    }

    #[test]
    fn test_task_complexity_target_percentile_is_monotonically_increasing() {
        let levels = [
            TaskComplexity::Trivial,
            TaskComplexity::Simple,
            TaskComplexity::Moderate,
            TaskComplexity::Complex,
            TaskComplexity::VeryComplex,
        ];
        for pair in levels.windows(2) {
            assert!(
                pair[0].target_percentile() < pair[1].target_percentile(),
                "{:?} ({}) must target a lower percentile than {:?} ({})",
                pair[0],
                pair[0].target_percentile(),
                pair[1],
                pair[1].target_percentile()
            );
        }
        assert_eq!(TaskComplexity::Trivial.target_percentile(), 0.0);
        assert_eq!(TaskComplexity::VeryComplex.target_percentile(), 1.0);
    }

    #[test]
    fn test_task_complexity_escalate_moves_up_one_level() {
        assert_eq!(TaskComplexity::Trivial.escalate(), TaskComplexity::Simple);
        assert_eq!(TaskComplexity::Simple.escalate(), TaskComplexity::Moderate);
        assert_eq!(TaskComplexity::Moderate.escalate(), TaskComplexity::Complex);
        assert_eq!(
            TaskComplexity::Complex.escalate(),
            TaskComplexity::VeryComplex
        );
    }

    #[test]
    fn test_task_complexity_escalate_saturates_at_very_complex() {
        assert_eq!(
            TaskComplexity::VeryComplex.escalate(),
            TaskComplexity::VeryComplex
        );
    }

    #[test]
    fn test_task_complexity_ord_matches_declared_severity_order() {
        assert!(TaskComplexity::Trivial < TaskComplexity::Simple);
        assert!(TaskComplexity::Simple < TaskComplexity::Moderate);
        assert!(TaskComplexity::Moderate < TaskComplexity::Complex);
        assert!(TaskComplexity::Complex < TaskComplexity::VeryComplex);
    }
}
