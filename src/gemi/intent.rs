use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentCategory {
    Coding,
    Mathematics,
    Reasoning,
    Creative,
    General,
}

impl Default for IntentCategory {
    fn default() -> Self {
        Self::General
    }
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
