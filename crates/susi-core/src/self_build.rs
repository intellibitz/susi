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

/// Baseline native local checks; production also requires artifact and rollback evidence.
pub const VERIFY_COMMAND: &str = "cargo fmt --all --check \
     && cargo clippy --workspace --all-targets --offline --locked -- -D warnings \
     && cargo test --workspace --offline --locked \
     && cargo deny --offline --locked check licenses";

/// The CEO's contract, prepended to every coding task in a Susi checkout.
/// Requirements are distinct from verified implementation status.
pub const BRIEF: &str = "\
[SUSI SELF-BUILD CONTRACT — identity.json Mandates 43-71; canonical rules in AGENTS.md]
The CEO never looks at code. Agents own authorized architecture, implementation, \
testing, debugging, dependency replacement, integration and production qualification \
end-to-end. Do not ask the CEO to inspect code or perform technical steps. \
The CEO grants standing authority to commit, push and merge without routine CEO approval; \
automated agent review, required tests and serialized integration remain mandatory. \
This Git authority does not waive production evidence gates or authorize unrelated spending, \
destructive operations or external publication.
1. Read AGENTS.md and live task/evidence state. Work in your own worktree and branch, \
with unique identity and disjoint scopes. Use cargo local-workflow for available \
local reservations and guarded commits. Never edit the primary checkout, share \
writable targets/runtime homes/ports, or change another agent's files. Keep one \
live claim per worker; parallelize independent tasks with isolated agents.
2. Susi runtime, build/install/release tooling and shipped implementation dependencies \
must be Rust. Windows must be native MSVC with no Bash, WSL, MSYS, Python, Node or \
container prerequisite. Replace incompatible dependencies without hiding them behind \
facades or silently dropping required capabilities. OS ABI/toolchain boundaries \
are explicit; remote services are not claims about the local implementation.
3. GitHub is optional. Local development, debug builds, integration and qualification \
must not require hosted CI, remote claims, origin/main, pull requests or human code \
review. Use cargo local-build debug for the native candidate path. Complete missing \
native workflow capabilities rather than claiming they exist.
4. Debug and optimized candidates stay separate from the installed production \
toolchain. Tests must be hermetic: private scratch HOME/USERPROFILE/XDG/SUSI_HOME, \
no production ~/.susi, shared ~/.susi-dev, inherited ports or real credentials. \
Never copy an unqualified candidate over production.
5. Allow only Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause for distributed \
components. Audit transitive dependencies, native objects, build helpers and model \
assets. Preserve upstream notices and evaluate AND/OR obligations correctly; never \
relabel third-party code, broaden the allowlist or hide shipped audit failures.
6. Zero bloat means measured size/memory/latency/build budgets and justified \
capabilities. Zero required user configuration means automatic discovery, defaults \
and agent repair. Credentials, privacy consent, spending and destructive-action \
authorization remain explicit. Do not invent permissions or secret values.
7. Verify locally with cargo fmt, clippy, required nonempty tests, license and Rust \
audits, then serialize integration and re-test the actual combined tree. Each commit \
has task ownership (Task: T-<AGENT>-<n>, or native Task: LOCAL-<name> during migration). \
Namespace evidence EV-<AGENT>-<n>; preserve others' history. A scope receipt or compile \
is not acceptance. Close only when the entire task's acceptance check passes on the \
verified local integration head; a partial result does not close a larger requirement.
8. Susi: AI that earns its next upgrade. Production promotion requires exact \
source/lock/toolchain/target evidence, executable smoke tests, artifact digest, \
atomic replacement, health checks and tested rollback to the previous verified \
artifact. Durable learning requires persisted and retrievable evidence. No model \
assertion or hosted tag certifies success. Keep unmet requirements open and continue \
repairing them; report actual evidence without declaring aspirational compliance.
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
            "Mandates 43-71",
            "CEO never looks at code",
            "cargo local-workflow",
            "cargo local-build debug",
            "native MSVC",
            "GitHub is optional",
            "Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause",
            "Zero bloat",
            "Zero required user configuration",
            "Task: T-<AGENT>-<n>",
            "EV-<AGENT>-<n>",
            "hermetic",
            "SUSI_HOME",
            "actual combined tree",
            "tested rollback",
            "acceptance check passes",
            "live claim per worker",
            "earns its next upgrade",
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
