//! The active self-build contract for code-writing tasks in this checkout.
//!
//! When the workspace an agent works in is the SUSI source tree itself,
//! every code-writing path — the native swarm, delegated external agents
//! and the self-patch cycle —
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

/// The active contract, prepended to every coding task in a Susi checkout.
/// Requirements are distinct from verified implementation status.
pub const BRIEF: &str = "\
[SUSI SELF-BUILD CONTRACT — canonical active rules in AGENTS.md]
Agents own authorized development, verification, integration and production qualification. \
Do not ask the requester to inspect code or perform routine engineering steps. Requirements \
are not evidence of completed implementation; keep unmet goals open.
1. Read AGENTS.md and active local state. Work under your own agent root in an isolated \
worktree and branch with unique identity and disjoint scopes. Use cargo local-workflow for \
available reservations and guarded commits, and cargo local-build debug for candidates. \
Never edit the primary or requester's checkout or another agent's files. Keep one live claim \
per worker, renew active progress, and release claims when work ends, is handed off or stops. \
Preserve unpublished work. Cleanup requires every commit on origin/main, a clean tree, no \
live claim or running agent, and no next-task reuse; remove only your own worktree, merged \
branch and private target, never shared caches or another owner's state.
2. Runtime, build/install/release tooling and shipped dependencies must be Rust. Windows \
uses native MSVC without Bash/WSL/MSYS, Python, Node or container prerequisites. Qualify only \
on the current x86_64 Windows or Linux host while preserving portable source. OS ABI and \
installed toolchains are explicit boundaries, not permission to hide foreign runtimes.
3. Hosting is optional. Local development and qualification need no hosted CI, pull request \
or remote review. Use portable Git storage and existing access controls. Agents may commit, \
push and merge authorized work without routine approval, using serialized integration and \
normal atomic fast-forward updates. Never force-push or overwrite shared history. Fetch and \
merge current origin/main when reachable before work, on resume and before committing. \
Commit, push, merge and pull about every ten minutes, meaningfully: a cadence against drift, \
not a hard limit; resolve conflicts, with no empty commits, noise or time-based refusal. \
This authority does not authorize unrelated spending, destructive operations or external \
publication. Send external messages only with explicit user or invoked-workflow authorization. \
When messaging is authorized, close coordination items by appending, never editing; post \
facts once, answer requests and reconcile stale items before leaving. Never lift another \
owner's stop without an authorized decision.
4. Main has no mandatory pre-push gates and is unqualified development; full exact-tree \
gates apply to release branches and snapshots. Verify the remote contains a commit before \
claiming publication. Only refs/heads/main is a remote branch; handoffs use refs/agents/ and \
release lines refs/candidates/release/. Release tags are immutable. Each commit carries \
Task: T-<AGENT>-<n> or native Task: LOCAL-<name>. A reservation or compile is not acceptance: \
close only after the entire acceptance check passes on the actual combined tree. Keep commits tidy.
5. Tests are hermetic: private HOME/USERPROFILE/XDG/SUSI_HOME, ports, local remotes and \
scratch configuration, no production state or real credentials. Clear child environments and \
prove fixture routing before mutation. Never install an unqualified candidate as production. \
Promotion requires exact source/lock/toolchain/target/feature evidence, nonempty tests, \
license and Rust audits, executable smoke tests, artifact digest, atomic replacement, health \
checks and tested rollback. Qualified releases also synchronize the installed binary, \
susi.build.json and live daemon. Keep Windows scheduled tasks at RunLevel=Limited. \
Keep the installed release current when a newer host-qualified release is reachable and \
no client is mid-mission; verify its artifact and retain rollback.
6. Keep core a generic standards socket with exactly three core pillars: GAWD (A2A, ACP and \
parallel harness), GEMI and GMCP. Concrete implementations belong in separate packages; \
prove each destination works through the standard socket before removing core implementation. \
No functionality is dropped. Core stays under 100,000 physical runtime Rust lines, including \
all platforms/features, comments, blanks, inline tests and facades; until then it never grows. \
Lower COMPLETE_SOCKET_CEILING after each reduction. Never hide or minify code or weaken a \
check to pass. Core has no UI or bundled plug content; keep proprietary code out of core.
7. Zero bloat requires measured source, dependency, artifact, memory, latency and build cost. \
Zero required user configuration means discovery, defaults and agent repair. The warm \
build + test + release budget is <10s, the cold compile target <60s. The delivery goal is a \
verified live executable in under 60 seconds with declared millisecond response budgets. \
Measure cold/warm build, test, startup, acknowledgment, first output and completion separately, \
including failures and rollback. Show plugin updates over the socket without rebuilding core.
8. Look for reuse inside both repositories, suitable Apache-2.0 work and the locked graph \
before building; delete what reuse replaces. Copied source must offer pure Apache-2.0, \
explicitly elected with pinned provenance and all license/modification notices. Distributed \
components allow Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause under the existing policy. \
Audit nested exceptions, transitive/native/build/model/standard-library sources and AND/OR \
obligations. Never relabel third-party code, broaden the allowlist or hide an audit failure. \
Keep dependency, unsafe and size ratchets, typed errors and justified narrow FFI boundaries.
9. Operational history, growing facts, evidence and completed records stay outside source, in a \
local database when available; history may be absent. Missing history never proves acceptance, \
dependency completion or qualification. Retain mandatory notices, live authority and recovery \
data and artifact-bound provenance. Keep active coordination and task records minimal, true and \
tidy; remove when done. Link and revisit unfulfilled goals; keep contract, docs, tests, code tidy.
10. Drivers and engines compose across every container component with capability contracts, \
cycle/termination checks and resource/time/depth budgets; nested work cannot amplify \
permissions. Cloud sessions remain within user authorization. Concurrency follows measured \
capacity; default-driver status requires parity. Preserve privacy consent, spending limits \
and destructive-action authorization. Reach secrets only through the authorized automation \
vault or scoped environment; never open a private personal vault or disclose a value. \
Keep credential/refusal compatibility until its implementation is safely migrated.
11. Core is Apache-2.0 and published one way. The designated publisher alone holds push \
authority; authorized agents act within that scope, with no additional write access. Accept \
no outside pull request, patch or commit. Everyone is free to fork under the license, which \
grants code rights, not the Susi name or marks. Runtime and current documentation stay \
generic; mandatory legal notices and protocol identifiers retain their original meaning.
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

    fn tree(cargo: &str, constitution: bool) -> susi_paths::tempfile::TempDir {
        let dir = susi_paths::tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), cargo).unwrap();
        if constitution {
            std::fs::create_dir_all(dir.path().join(".agents")).unwrap();
            std::fs::write(dir.path().join(".agents/identity.json"), "{}").unwrap();
        }
        dir
    }

    #[test]
    fn the_contract_carries_active_boundaries_without_identity_history() {
        for needle in [
            "canonical active rules in AGENTS.md",
            "cargo local-workflow",
            "cargo local-build debug",
            "native MSVC",
            "Hosting is optional",
            "hermetic",
            "SUSI_HOME",
            "one live claim",
            "release claims when work ends",
            "Preserve unpublished work",
            "no live claim or running agent",
            "never shared caches",
            "without routine approval",
            "Never force-push",
            "external publication",
            "Send external messages only with explicit",
            "normal atomic fast-forward",
            "Main has no mandatory pre-push gates",
            "gates apply to release branches",
            "Release tags are immutable",
            "Task: T-<AGENT>-<n>",
            "actual combined tree",
            "entire acceptance check passes",
            "tested rollback",
            "artifact digest",
            "susi.build.json and live daemon",
            "RunLevel=Limited",
            "exactly three core pillars",
            "No functionality is dropped",
            "under 100,000 physical runtime Rust lines",
            "until then it never grows",
            "destination works through the standard socket",
            "Lower COMPLETE_SOCKET_CEILING",
            "Zero bloat",
            "Zero required user configuration",
            "pure Apache-2.0",
            "Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause",
            "Never relabel third-party code",
            "outside source",
            "history may be absent",
            "Missing history never proves",
            "nested work cannot amplify permissions",
            "Cloud sessions",
            "never open a private personal vault",
            "published one way",
            "designated publisher alone holds push",
            "no additional write access",
            "no outside pull request, patch or commit",
            "free to fork",
        ] {
            assert!(BRIEF.contains(needle), "BRIEF must mention `{needle}`");
        }
        for historical in ["CEO", "intellibitz", "Mandate", "dot, opus", "EV-<AGENT>"] {
            assert!(
                !BRIEF.contains(historical),
                "identity/history returned: {historical}"
            );
        }
    }

    #[test]
    fn delegated_delivery_criteria_match_the_active_checkout_contract() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let governance = include_str!("../../../AGENTS.md");
        let delegated = brief_task(&root, "measure the delivery loop");
        for criterion in [
            "verified live executable in under 60 seconds",
            "declared millisecond response budgets",
            "failures and rollback",
            "plugin updates over the socket without rebuilding core",
        ] {
            assert!(governance.contains(criterion));
            assert!(delegated.contains(criterion));
        }
        assert!(delegated.ends_with("measure the delivery loop"));
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
