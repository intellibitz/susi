//! Goal-shape classifiers shared by native agents and fleet synthesis.
//!
//! Kept free of `GawdAgentFleet` so native agent impls do not form a module cycle
//! with the fleet synthesizer (Mandate 3 readability split).

/// Substrate meta-commands only (identity/status/models/version/admin/
/// ls/dir/whoami) — deliberately narrower than [`is_meta_or_simple_query`]
/// and does NOT treat question-shaped goals as a match.
pub fn is_meta_command(goal: &str) -> bool {
    let lower_goal = goal.trim().to_lowercase();
    lower_goal.contains("admin")
        || lower_goal.contains("identity")
        || lower_goal.contains("status")
        || lower_goal.contains("models")
        || lower_goal.contains("version")
        || lower_goal == "ls"
        || lower_goal.starts_with("ls ")
        || lower_goal == "dir"
        || lower_goal.contains("who am i")
        || lower_goal.contains("whoami")
}

fn is_plain_question(goal: &str) -> bool {
    let lower_goal = goal.trim().to_lowercase();
    lower_goal.ends_with('?')
        || [
            "what ", "who ", "when ", "where ", "why ", "how ", "which ", "is ", "are ", "does ",
            "do ", "can ", "could ", "will ", "would ",
        ]
        .iter()
        .any(|w| lower_goal.starts_with(w))
}

/// Meta-command **or** ordinary natural-language question — either way,
/// inventing a specialist agent via Neural Agent Synthesis is usually waste.
pub fn is_meta_or_simple_query(goal: &str) -> bool {
    is_meta_command(goal) || is_plain_question(goal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_commands_match() {
        assert!(is_meta_command("admin pulse: sync"));
        assert!(is_meta_command("susi identity"));
        assert!(is_meta_command("status"));
        assert!(is_meta_command("ls"));
        assert!(is_meta_command("whoami"));
    }

    #[test]
    fn translation_question_is_simple_not_meta() {
        let q = "How do you say hello in French?";
        assert!(!is_meta_command(q));
        assert!(is_meta_or_simple_query(q));
    }
}
