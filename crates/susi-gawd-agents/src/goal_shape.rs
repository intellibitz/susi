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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
mod contract_tests {
    use super::*;

    #[test]
    fn meta_command_keywords_match_case_insensitively_after_trimming() {
        for goal in [
            "admin",
            "ADMIN pulse",
            "show identity",
            "models",
            "Models please",
            "version",
            "  VERSION  ",
            "who am i",
            "Who Am I?",
            "whoami",
            "status",
        ] {
            assert!(is_meta_command(goal), "{goal:?}");
        }
    }

    #[test]
    fn ls_and_dir_only_match_as_whole_commands() {
        assert!(is_meta_command("ls"));
        assert!(is_meta_command("LS"));
        assert!(is_meta_command("  ls  "));
        assert!(is_meta_command("ls -la"));
        assert!(is_meta_command("ls src/"));
        assert!(is_meta_command("dir"));
        // Boundary: neither is a prefix match of a longer word.
        for goal in ["list files", "lsof", "directory", "dirty tree", "also"] {
            assert!(!is_meta_command(goal), "{goal:?}");
        }
    }

    #[test]
    fn imperative_engineering_goals_are_neither_meta_nor_simple() {
        for goal in [
            "refactor the parser into two modules",
            "implement retry logic for the scheduler",
            "fix the failing build",
            "add unit tests for the router",
            "build a release binary",
            "migrate the schema",
        ] {
            assert!(!is_meta_command(goal), "{goal:?}");
            assert!(!is_plain_question(goal), "{goal:?}");
            assert!(!is_meta_or_simple_query(goal), "{goal:?}");
        }
    }

    #[test]
    fn every_question_prefix_marks_a_plain_question_without_a_question_mark() {
        for prefix in [
            "what ", "who ", "when ", "where ", "why ", "how ", "which ", "is ", "are ", "does ",
            "do ", "can ", "could ", "will ", "would ",
        ] {
            let goal = format!("{prefix}the cache layer work");
            assert!(is_plain_question(&goal), "{goal:?}");
            assert!(is_meta_or_simple_query(&goal), "{goal:?}");
            assert!(
                is_plain_question(&goal.to_uppercase()),
                "{goal:?} uppercase"
            );
        }
    }

    #[test]
    fn question_prefixes_need_the_trailing_space_to_count() {
        for goal in [
            "whatever you decide",
            "island hopping",
            "dozen eggs",
            "cancel the job",
            "whole system rewrite",
            "willow",
        ] {
            assert!(!is_plain_question(goal), "{goal:?}");
        }
    }

    #[test]
    fn a_trailing_question_mark_alone_makes_a_question() {
        for goal in ["deploy now?", "ready?  ", "  ok?\n", "?"] {
            assert!(is_plain_question(goal), "{goal:?}");
            assert!(is_meta_or_simple_query(goal), "{goal:?}");
        }
        assert!(!is_plain_question(""));
        assert!(!is_plain_question("   "));
    }

    #[test]
    fn a_meta_keyword_inside_a_question_is_both_meta_and_simple() {
        let goal = "what is the version?";
        assert!(is_meta_command(goal));
        assert!(is_plain_question(goal));
        assert!(is_meta_or_simple_query(goal));
    }
}
