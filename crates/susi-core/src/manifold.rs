// Classifies an intent string's scope of impact and risk level via keyword
// matching, to decide whether it needs swarm dispatch and verification.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeOfImpact {
    Read,       // Zero-mutation query
    Write,      // Workspace file I/O
    Mutate,     // Substrate modification / admin
    SelfExtend, // Reflex synthesis / code evolution
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskProfile {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentManifold {
    pub raw_intent: String,
    pub scope_of_impact: ScopeOfImpact,
    pub risk_profile: RiskProfile,
    pub requires_swarm: bool,
    pub requires_verification: bool,
}

impl IntentManifold {
    pub fn analyze(intent: &str) -> Self {
        let lower = intent.to_lowercase();
        // CLI bare intents are often prefixed with a verb token (`run …`).
        // Strip it so "run Read Cargo.toml … Cite the file." stays Read —
        // otherwise the trailing "file" word forces Write and burns a swarm.
        let focused = lower
            .strip_prefix("run ")
            .or_else(|| lower.strip_prefix("automate "))
            .unwrap_or(lower.as_str());

        let scope_of_impact = if is_workspace_file_read(focused)
            || is_prose_answer_request(focused)
            || focused.trim() == "identity"
            || focused.trim() == "status"
            || focused.trim() == "susi status"
            || focused.trim() == "models"
            || focused.trim() == "list models"
            || focused.trim() == "version"
            || focused.trim() == "susi version"
            || focused.trim() == "dashboard"
            || focused.trim() == "bloat audit"
            || focused == "ls"
            || focused.starts_with("ls ")
            || focused == "dir"
            || focused == "list directory"
            || focused == "list files"
            || focused == "who am i"
            || focused == "whoami"
        {
            ScopeOfImpact::Read
        } else if focused.contains("write")
            || focused.contains("save")
            || focused.contains("edit")
            || focused.contains("file")
        {
            ScopeOfImpact::Write
        } else if focused.contains("admin")
            || focused.contains("sync")
            || focused.contains("audit")
            || focused.contains("release")
            || focused.contains("verify")
        {
            ScopeOfImpact::Mutate
        } else if is_reasoning_query(focused) {
            ScopeOfImpact::Read
        } else {
            ScopeOfImpact::SelfExtend
        };

        let risk_profile = if focused.contains("rm -rf")
            || focused.contains("drop")
            || focused.contains("delete /")
        {
            RiskProfile::Critical
        } else if focused.contains("exec")
            || focused.contains("command")
            || focused.contains("install")
        {
            RiskProfile::High
        } else if scope_of_impact == ScopeOfImpact::Mutate {
            RiskProfile::Medium
        } else {
            RiskProfile::Low
        };

        let requires_swarm = scope_of_impact != ScopeOfImpact::Read;
        let requires_verification = scope_of_impact != ScopeOfImpact::Read;

        Self {
            raw_intent: intent.to_string(),
            scope_of_impact,
            risk_profile,
            requires_swarm,
            requires_verification,
        }
    }
}

/// Producing prose in the reply does not mutate the workspace. Keep this
/// narrow: an explicit prose noun must follow the composition verb, and any
/// requested file, tool, publication or system action retains governed routing.
fn is_prose_answer_request(focused: &str) -> bool {
    let words: Vec<_> = focused
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    // This shortcut accepts only one composition request. Do not infer the
    // safety of a following clause from a finite list of mutation verbs:
    // unfamiliar actions after a conjunction still need normal governance.
    let body = focused.trim().trim_end_matches(['.', '!', '?']);
    if body
        .chars()
        .any(|c| matches!(c, ';' | ',' | '.' | '!' | '?' | '&' | '\n' | '\r'))
        || words.iter().any(|word| {
            matches!(
                *word,
                "and"
                    | "then"
                    | "or"
                    | "also"
                    | "next"
                    | "after"
                    | "before"
                    | "afterward"
                    | "afterwards"
                    | "subsequently"
                    | "followed"
                    | "while"
            )
        })
    {
        return false;
    }
    let Some(first) = words.first() else {
        return false;
    };
    if !matches!(*first, "write" | "draft" | "compose") {
        return false;
    }
    let noun = words.iter().skip(1).find(|word| {
        !matches!(
            **word,
            "a" | "an" | "one" | "short" | "brief" | "single" | "simple" | "creative"
        )
    });
    if !noun.is_some_and(|word| {
        matches!(
            *word,
            "sentence"
                | "paragraph"
                | "poem"
                | "haiku"
                | "story"
                | "summary"
                | "description"
                | "explanation"
                | "reply"
        )
    }) {
        return false;
    }
    if focused.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '\\');
        word.contains('/') || word.contains('\\') || word.contains('.')
    }) {
        return false;
    }
    !words.iter().skip(1).any(|word| {
        matches!(
            *word,
            "file"
                | "files"
                | "folder"
                | "directory"
                | "disk"
                | "workspace"
                | "repository"
                | "into"
                | "save"
                | "append"
                | "overwrite"
                | "edit"
                | "modify"
                | "delete"
                | "remove"
                | "install"
                | "uninstall"
                | "execute"
                | "run"
                | "command"
                | "script"
                | "code"
                | "deploy"
                | "publish"
                | "send"
                | "email"
                | "download"
                | "upload"
                | "commit"
                | "push"
                | "rename"
                | "move"
                | "update"
                | "create"
                | "build"
                | "implement"
                | "write"
                | "sync"
                | "release"
        )
    })
}

/// Explicit answer requests need inference, not code evolution. Keep action
/// verbs out of this route even when appended to a question.
fn is_reasoning_query(focused: &str) -> bool {
    let words: Vec<_> = focused
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|word| {
        matches!(
            *word,
            "create"
                | "build"
                | "implement"
                | "modify"
                | "change"
                | "delete"
                | "remove"
                | "install"
                | "uninstall"
                | "execute"
                | "run"
                | "deploy"
                | "publish"
                | "send"
                | "download"
                | "upload"
                | "commit"
                | "push"
                | "rename"
                | "move"
                | "update"
                | "write"
                | "into"
                | "save"
                | "edit"
        )
    }) {
        return false;
    }
    matches!(
        words.first().copied(),
        Some(
            "compute"
                | "calculate"
                | "explain"
                | "summarize"
                | "summarise"
                | "describe"
                | "compare"
                | "answer"
                | "what"
                | "why"
                | "how"
                | "when"
                | "where"
                | "which"
                | "who"
        )
    )
}

/// `read Cargo.toml` / `cat src/lib.rs` — zero-mutation path-shaped queries.
fn is_workspace_file_read(focused: &str) -> bool {
    let rest = focused
        .trim()
        .strip_prefix("read ")
        .or_else(|| focused.trim().strip_prefix("cat "));
    let Some(rest) = rest else {
        return false;
    };
    let token = rest.split_whitespace().next().unwrap_or("");
    !token.is_empty() && (token.contains('.') || token.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_replies_use_inference_without_starting_a_swarm() {
        for goal in [
            "Write one short sentence about a red kite.",
            "Draft a brief paragraph about rain",
            "Compose a haiku about winter",
        ] {
            let intent = IntentManifold::analyze(goal);
            assert_eq!(intent.scope_of_impact, ScopeOfImpact::Read, "{goal}");
            assert!(!intent.requires_swarm, "{goal}");
        }
        for goal in [
            "Write a sentence and restart the server",
            "Draft a summary then merge the branches",
            "Compose a poem; restart the server",
            "Write a sentence. Restart the server",
            "Draft a summary, merge the branches",
            "Write a sentence & restart the server",
            "Write a sentence\nrestart the server",
            "Write one sentence and save it to notes.md",
            "Write one sentence to notes.md",
            "Write a paragraph into the report",
            "Draft a paragraph then send it",
            "Compose a poem and delete old files",
            "Write a file named notes.md",
            "Write code for a calculator",
            "Write a sentence then run a command",
            "Write a story and publish it",
        ] {
            assert!(IntentManifold::analyze(goal).requires_swarm, "{goal}");
        }
    }

    #[test]
    fn first_goal_questions_use_one_read_path_but_actions_keep_governance() {
        for goal in [
            "Compute 17 + 25. Answer with only the number.",
            "Explain photosynthesis",
            "Summarize this paragraph: a quiet morning",
            "What is 9 times 6?",
        ] {
            let intent = IntentManifold::analyze(goal);
            assert_eq!(intent.scope_of_impact, ScopeOfImpact::Read, "{goal}");
            assert!(!intent.requires_swarm);
        }
        for goal in [
            "Explain then install Python",
            "Compute 17 + 25 and write the answer",
            "What is next? Delete the project",
            "Build a calculator",
            "Unknown request",
        ] {
            assert!(IntentManifold::analyze(goal).requires_swarm, "{goal}");
        }
    }

    #[test]
    fn run_read_cargo_toml_is_read_not_write() {
        let m = IntentManifold::analyze(
            "run Read Cargo.toml in this workspace and reply with only the package name from [package]. Cite the file.",
        );
        assert_eq!(m.scope_of_impact, ScopeOfImpact::Read);
        assert!(!m.requires_swarm);
    }

    #[test]
    fn bare_read_path_is_read() {
        let m = IntentManifold::analyze("read src/main.rs");
        assert_eq!(m.scope_of_impact, ScopeOfImpact::Read);
    }

    #[test]
    fn write_file_still_write() {
        let m = IntentManifold::analyze("write a file named notes.md");
        assert_eq!(m.scope_of_impact, ScopeOfImpact::Write);
    }
}
