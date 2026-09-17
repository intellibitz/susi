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

pub struct IntentClassifier;

impl IntentClassifier {
    /// Zero-latency heuristic intent classification based on semantic triggers.
    pub fn classify(prompt: &str) -> IntentCategory {
        let text = prompt.to_lowercase();
        
        let code_triggers = ["code", "impl", "rust", "python", "fn ", "def ", "class ", "function", "struct ", "script", "debug", "error:", "panic!", "compiler"];
        let math_triggers = ["math", "equation", "calculus", "algebra", "integral", "derivative", "solve for", "theorem", "matrix"];
        let logic_triggers = ["analyze", "reason", "evaluate", "why is", "compare", "contrast", "deduce", "logic", "explain how"];
        let creative_triggers = ["write a poem", "story", "imagine", "creative", "generate an image", "roleplay", "write a song"];

        let count_hits = |triggers: &[&str]| -> usize {
            triggers.iter().filter(|&&t| text.contains(t)).count()
        };

        let code_score = count_hits(&code_triggers);
        let math_score = count_hits(&math_triggers);
        let logic_score = count_hits(&logic_triggers);
        let creative_score = count_hits(&creative_triggers);

        let max_score = *[code_score, math_score, logic_score, creative_score, 0].iter().max().unwrap_or(&0);

        if max_score == 0 {
            return IntentCategory::General;
        }

        if code_score == max_score && code_score > 0 { return IntentCategory::Coding; }
        if math_score == max_score && math_score > 0 { return IntentCategory::Mathematics; }
        if logic_score == max_score && logic_score > 0 { return IntentCategory::Reasoning; }
        if creative_score == max_score && creative_score > 0 { return IntentCategory::Creative; }

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
            IntentClassifier::classify("Analyze and compare these two theories, then explain how they differ"),
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
}
