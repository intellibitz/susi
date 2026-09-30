//! Who acts on a served Tier-0/1 `ACTION:` line.
//!
//! A reflex classifier emits `ACTION: <name> [args]` — previously the
//! literal text reached the user. This module picks option (a) from
//! T-CLAUDE-8: *read-only foundational actions* are executed through the
//! tools plane and the answer carries a `reflex:*` receipt; everything
//! else is escalated, never executed. A client may instead opt in to the
//! ACTION protocol and receive the raw action.
//!
//! Execution is injected via [`ActionTool`] so the decision logic is
//! tested without tools, MAC, or the reflex model.

use serde::{Deserialize, Serialize};

/// A parsed `ACTION: <name> [args]` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAction {
    pub name: String,
    pub args: Vec<String>,
}

/// Parse a served reflex line. Returns `None` for generative (non-ACTION)
/// output and for a bare `ACTION:` with no name.
#[must_use]
pub fn parse_action(line: &str) -> Option<ParsedAction> {
    let rest = line.trim().strip_prefix("ACTION:")?.trim();
    let mut it = rest.split_whitespace();
    let name = it.next()?.to_ascii_lowercase();
    Some(ParsedAction {
        name,
        args: it.map(str::to_string).collect(),
    })
}

/// Read-only foundational actions the reflex may run end-to-end. Anything
/// not in this table is never executed from a reflex line.
const READ_ONLY_ACTIONS: &[&str] = &[
    "status",
    "version",
    "list_directory",
    "list_models",
    "help",
    "uptime",
];

/// `true` when `name` is a read-only foundational action.
#[must_use]
pub fn is_read_only(name: &str) -> bool {
    READ_ONLY_ACTIONS.contains(&name)
}

/// The tools-plane surface the executor needs — one entry point that runs
/// a named read-only action with args and returns display text. In
/// production this wraps `susi-tools`; tests inject a stub.
pub trait ActionTool {
    /// Execute `name` with `args`; must not perform writes for the names
    /// in `READ_ONLY_ACTIONS`.
    fn run_read_only(&self, name: &str, args: &[String]) -> Result<String, String>;
}

/// What the served line resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionOutcome {
    /// Read-only action executed; `output` is the tool's answer.
    Executed { receipt: String, output: String },
    /// Non-read-only action — escalated for a full mission; never run.
    Escalated { action: String },
    /// The client opted in to the raw ACTION protocol.
    PassedThrough { line: String },
    /// Not an ACTION line / unparseable — treat as generative text.
    Generative,
}

/// Decide (and for read-only, execute) a served Tier-0/1 line.
///
/// `client_accepts_action` marks a client that opted in to the ACTION
/// protocol: it gets the raw line back regardless of action kind.
pub fn resolve(
    served_line: &str,
    client_accepts_action: bool,
    tool: &dyn ActionTool,
) -> ActionOutcome {
    if client_accepts_action {
        return ActionOutcome::PassedThrough {
            line: served_line.trim().to_string(),
        };
    }
    let Some(action) = parse_action(served_line) else {
        return ActionOutcome::Generative;
    };
    if !is_read_only(&action.name) {
        return ActionOutcome::Escalated {
            action: format!("{} {}", action.name, action.args.join(" "))
                .trim()
                .to_string(),
        };
    }
    match tool.run_read_only(&action.name, &action.args) {
        Ok(output) => ActionOutcome::Executed {
            receipt: format!("reflex:{}", action.name),
            output,
        },
        Err(e) => ActionOutcome::Escalated {
            action: format!("{} (tool error: {e})", action.name),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct StubTool {
        calls: RefCell<Vec<String>>,
    }
    impl StubTool {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl ActionTool for StubTool {
        fn run_read_only(&self, name: &str, args: &[String]) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(format!("{name} {}", args.join(" ")));
            if name == "list_directory" {
                Ok("src/\nCargo.toml".to_string())
            } else {
                Ok(format!("{name} ok"))
            }
        }
    }

    #[test]
    fn action_executor_parses_lines() {
        let p = parse_action("ACTION: status").unwrap();
        assert_eq!(p.name, "status");
        assert!(p.args.is_empty());
        let p = parse_action("ACTION: list_directory /ws").unwrap();
        assert_eq!(p.args, vec!["/ws"]);
        assert!(parse_action("ACTION:").is_none());
        assert!(parse_action("hello world").is_none());
    }

    #[test]
    fn action_executor_runs_read_only_with_receipt() {
        let tool = StubTool::new();
        let out = resolve("ACTION: status", false, &tool);
        match out {
            ActionOutcome::Executed { receipt, output } => {
                assert_eq!(receipt, "reflex:status");
                assert_eq!(output, "status ok");
            }
            other => panic!("expected Executed, got {other:?}"),
        }
        assert_eq!(tool.calls.borrow().len(), 1);
    }

    #[test]
    fn action_executor_list_directory_passes_args() {
        let tool = StubTool::new();
        let out = resolve("ACTION: list_directory /ws", false, &tool);
        match out {
            ActionOutcome::Executed { output, .. } => assert!(output.contains("src/")),
            other => panic!("expected Executed, got {other:?}"),
        }
        assert_eq!(tool.calls.borrow()[0], "list_directory /ws");
    }

    #[test]
    fn action_executor_never_runs_mutating_action() {
        let tool = StubTool::new();
        for line in [
            "ACTION: delete_file /tmp/x",
            "ACTION: run_shell rm -rf /",
            "ACTION: write_file a b",
            "ACTION: deploy",
        ] {
            let out = resolve(line, false, &tool);
            match out {
                ActionOutcome::Escalated { action } => {
                    assert!(action.starts_with(
                        line.trim_start_matches("ACTION: ")
                            .split(' ')
                            .next()
                            .unwrap_or("")
                    ));
                }
                other => panic!("{line} must escalate, got {other:?}"),
            }
        }
        assert!(tool.calls.borrow().is_empty(), "mutating action executed!");
    }

    #[test]
    fn action_executor_passthrough_when_client_opts_in() {
        let tool = StubTool::new();
        let out = resolve("ACTION: delete_file /x", true, &tool);
        match out {
            ActionOutcome::PassedThrough { line } => assert_eq!(line, "ACTION: delete_file /x"),
            other => panic!("expected PassedThrough, got {other:?}"),
        }
        assert!(tool.calls.borrow().is_empty());
    }

    #[test]
    fn action_executor_generative_text_untouched() {
        let tool = StubTool::new();
        assert_eq!(
            resolve("The answer is 42.", false, &tool),
            ActionOutcome::Generative
        );
        assert_eq!(resolve("ACTION:", false, &tool), ActionOutcome::Generative);
    }

    #[test]
    fn action_executor_tool_failure_escalates() {
        struct Failing;
        impl ActionTool for Failing {
            fn run_read_only(&self, _n: &str, _a: &[String]) -> Result<String, String> {
                Err("sandbox denied".to_string())
            }
        }
        match resolve("ACTION: status", false, &Failing) {
            ActionOutcome::Escalated { action } => assert!(action.contains("tool error")),
            other => panic!("expected Escalated, got {other:?}"),
        }
    }
}
