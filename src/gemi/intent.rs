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

/// Whether a prompt likely needs more than the smallest resident model, or
/// the baseline tier can probably handle it. Orthogonal to `IntentCategory`
/// (domain vs. difficulty) - a one-line arithmetic question and a one-line
/// "write a poem" prompt are both short/simple even though they'd classify
/// into different `IntentCategory` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TaskComplexity {
    Simple,
    #[default]
    Complex,
}

pub struct IntentClassifier;

impl IntentClassifier {
    /// Zero-latency heuristic for whether a prompt is likely simple enough
    /// for the small resident baseline model, used to pick a *starting*
    /// tier instead of always defaulting to the biggest model available
    /// (see `ModelManager::identify_best_suited_local_model`'s
    /// `complexity` parameter). This is a starting-tier guess, not a
    /// correctness guarantee - there is no cheap way to know in advance
    /// whether a model can actually solve a given prompt. Deliberately
    /// biased toward `Complex` (today's existing "prefer the biggest
    /// model" behavior) except for prompts that clearly look trivial:
    /// misclassifying a simple prompt as complex just costs a bit more
    /// compute on a model that's already resident; the reverse risks a
    /// materially worse answer for something that actually needed the
    /// bigger model.
    pub fn classify_complexity(prompt: &str) -> TaskComplexity {
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

        if word_count <= 12 && !has_multi_step_marker && question_count <= 1 && sentence_count <= 1
        {
            TaskComplexity::Simple
        } else {
            TaskComplexity::Complex
        }
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
    fn test_classify_complexity_short_single_question_is_simple() {
        assert_eq!(
            IntentClassifier::classify_complexity("What is the capital of France?"),
            TaskComplexity::Simple
        );
    }

    #[test]
    fn test_classify_complexity_greeting_is_simple() {
        assert_eq!(
            IntentClassifier::classify_complexity("hi there"),
            TaskComplexity::Simple
        );
    }

    #[test]
    fn test_classify_complexity_long_prompt_is_complex() {
        let prompt = "Design a distributed rate limiter that works correctly across \
            multiple servers without a single point of failure, handles clock skew \
            between nodes, and degrades gracefully under network partitions.";
        assert_eq!(
            IntentClassifier::classify_complexity(prompt),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_classify_complexity_multi_step_marker_forces_complex_even_if_short() {
        assert_eq!(
            IntentClassifier::classify_complexity("explain this step by step"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_classify_complexity_multiple_questions_is_complex() {
        assert_eq!(
            IntentClassifier::classify_complexity("What is Rust? Why use it?"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_classify_complexity_defaults_to_complex_not_simple_when_ambiguous() {
        // Regression: complexity must be biased toward Complex (today's
        // existing "prefer the biggest model" behavior) for anything that
        // isn't clearly trivial - a wrong "simple" guess risks a
        // materially worse answer, a wrong "complex" guess just costs a
        // bit more compute on an already-resident model.
        assert_eq!(TaskComplexity::default(), TaskComplexity::Complex);
    }
}
