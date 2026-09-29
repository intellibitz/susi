//! "SUSI writes SUSI": the self-build contract (identity.json Mandate 48).
//!
//! When the workspace an agent works in is the SUSI source tree itself,
//! every code-writing path — the native swarm, delegated external agents
//! (Claude Code, Codex, aider, Gemini CLI, …), and the self-patch cycle —
//! must follow the same order. Some external agents read `AGENTS.md` on
//! their own and some do not, so SUSI states the contract in the task
//! itself instead of hoping each tool finds it.

use std::path::Path;

/// Whether `workspace` is a SUSI source checkout: a root `Cargo.toml`
/// declaring `name = "susi"` next to the `.agents/identity.json` constitution.
#[must_use]
pub fn is_susi_repo(workspace: &Path) -> bool {
    if !workspace.join(".agents").join("identity.json").is_file() {
        return false;
    }
    std::fs::read_to_string(workspace.join("Cargo.toml")).is_ok_and(|toml| {
        toml.lines()
            .map(str::trim)
            .any(|line| line.replace(' ', "") == "name=\"susi\"")
    })
}

/// The gate a change to SUSI must pass (AGENTS.md "Verify before pushing").
pub const VERIFY_COMMAND: &str = "cargo fmt --all --check \
     && cargo clippy --workspace --all-targets --locked -- -D warnings \
     && cargo test --workspace --locked";

/// The contract, prepended to every task an agent receives in a SUSI tree.
pub const BRIEF: &str = "\
[SUSI SELF-BUILD CONTRACT — identity.json Mandates 48-56; full rules in AGENTS.md]
You are changing SUSI's own source. The installed ~/.susi/bin/susi is the release \
toolchain running this work; do not break it.
1. Build dev only into target/ (cargo build, cargo xb, ./build-gpu.sh). Never copy, \
install, or symlink a binary into ~/.susi/bin, and never run install.sh or \
scripts/susi-release-sync.sh.
2. Verify on the dev instance: run target/debug/susi or target/release/susi (it uses \
~/.susi-dev and ports 9190-9194 automatically). Do not point it at ~/.susi or \
ports 9090-9094.
3. Before finishing, pass: cargo fmt --all --check && cargo clippy --workspace \
--all-targets --locked -- -D warnings && cargo test --workspace --locked. \
Report the real result.
4. Commit on a branch with a conventional-commit message; add an .agents/evidence.json \
entry for behavior changes (id EV-<AGENT>-<n>). Do not push to main, create tags, or cut \
a release — releases are cut by the operator (susi admin release) and promoted to this \
host automatically.
5. Work in your own git worktree on your own branch (scripts/susi-worktree.sh <name>); \
never commit in the primary checkout or on main (Mandate 49).
6. Work from the task queue (Mandate 50): `susi tasks add '<title>' --accept '<cmd>'`, \
`susi tasks claim <id>` before starting, `susi tasks close <id>` only when its \
acceptance check passes. Never start a task another agent has claimed. End EVERY \
commit message with the trailer `Task: T-<AGENT>-<n>`; the commit-msg hook, the \
pre-push hook and the CI job 'Workflow Compliance' reject commits without it.
7. Never edit a line another agent owns (Mandate 51): add files/entries, do not \
rewrite or renumber others'; merge origin/main before pushing. Tests must be hermetic \
(Mandate 52): never read or write ~/.susi, ~/.susi-dev or an inherited SUSI_HOME.
[END CONTRACT]

";

/// `task` with [`BRIEF`] prepended when `workspace` is a SUSI tree;
/// unchanged otherwise.
#[must_use]
pub fn brief_task(workspace: &Path, task: &str) -> String {
    if is_susi_repo(workspace) && !task.starts_with(BRIEF) {
        format!("{BRIEF}{task}")
    } else {
        task.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(cargo: &str, constitution: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), cargo).unwrap();
        if constitution {
            std::fs::create_dir_all(dir.path().join(".agents")).unwrap();
            std::fs::write(dir.path().join(".agents/identity.json"), "{}").unwrap();
        }
        dir
    }

    #[test]
    fn the_contract_carries_the_workflow_mandates_agents_must_follow() {
        for needle in [
            "Mandates 48-56",
            "scripts/susi-worktree.sh",
            "susi tasks claim",
            "susi tasks close",
            "acceptance check passes",
            "Task: T-<AGENT>-<n>",
            "Workflow Compliance",
            "EV-<AGENT>-<n>",
            "hermetic",
            "SUSI_HOME",
        ] {
            assert!(BRIEF.contains(needle), "BRIEF must mention `{needle}`");
        }
    }

    #[test]
    fn only_a_susi_tree_gets_the_contract() {
        let susi = tree("[package]\nname = \"susi\"\n", true);
        let other = tree("[package]\nname = \"other\"\n", true);
        let no_constitution = tree("[package]\nname = \"susi\"\n", false);

        assert!(brief_task(susi.path(), "fix x").starts_with(BRIEF));
        assert_eq!(brief_task(other.path(), "fix x"), "fix x");
        assert_eq!(brief_task(no_constitution.path(), "fix x"), "fix x");
    }

    #[test]
    fn the_contract_is_not_stacked() {
        let susi = tree("[package]\nname = \"susi\"\n", true);
        let once = brief_task(susi.path(), "fix x");
        assert_eq!(brief_task(susi.path(), &once), once);
    }

    #[test]
    fn this_checkout_is_a_susi_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(is_susi_repo(&root));
    }
}
