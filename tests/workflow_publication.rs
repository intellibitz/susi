//! Exercise the actual atomic publisher against a disposable bare Git remote.
// Optional POSIX/hosted compatibility fixtures execute shell scripts.
// Native Windows delivery is covered by the Rust coordinator, bootstrap,
// installer and release suites; it must not require a POSIX userland.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn publication_case(existing_branch: &str, completion: &str) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("susi-publication-{}-{unique}", std::process::id())),
    );
    std::fs::create_dir_all(&fixture.0).unwrap();
    let home = fixture.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = home.join("gitconfig");
    std::fs::write(&config, "").unwrap();
    let workflow = if completion == "committed" {
        // Legacy committed records must not require a newer native binary.
        fixture.0.join("no-workflow-binary")
    } else {
        std::fs::canonicalize(std::env::var_os("SUSI_WORKFLOW_BIN").expect(
            "set SUSI_WORKFLOW_BIN to a native binary supporting tasks completed --publishable",
        ))
        .expect("SUSI_WORKFLOW_BIN must name an existing native binary")
    };
    let bash = if cfg!(windows) {
        PathBuf::from("C:/Program Files/Git/bin/bash.exe")
    } else {
        PathBuf::from("bash")
    };
    let publisher = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/publish-task-branch.sh")
        .to_string_lossy()
        .replace('\\', "/");
    let script = r#"
set -euo pipefail
publisher=$1
existing=$2
completion=$3
fixture=$PWD
git init --bare --quiet server.git
git init --quiet -b fixture branch
cd branch
git config user.name fixture
git config user.email fixture@localhost
git config commit.gpgsign false
git config core.hooksPath nonexistent-hooks
git config gc.auto 0
git config maintenance.auto false
git remote add origin "$fixture/server.git"
git commit --quiet --allow-empty -m accepted
accepted=$(git rev-parse HEAD)
task=T-FIXTURE-1
if [ "$completion" = committed ]; then
    mkdir -p .agents/tasks/done
    jq -n --arg head "$accepted" '{id:"T-FIXTURE-1",closed:{by:"FIXTURE",commit:$head,at_unix:1}}' > ".agents/tasks/done/$task.json"
    git add .agents
    git commit --quiet -m closure
elif [ "$completion" != missing ]; then
    records="$(git rev-parse --path-format=absolute --git-common-dir)/susi-records/completed"
    mkdir -p "$records"
    jq -n --arg head "$accepted" '{id:"T-FIXTURE-1",title:"accepted fixture",accept:{cmd:["cargo","--version"]},created_by:"FIXTURE",created_unix:1,closed:{by:"FIXTURE",commit:$head,at_unix:2}}' > "$records/$task.json"
    original=$PWD
    accepted_branch=fixture
    if [ "$completion" = wrong-worktree ]; then
        git worktree add --quiet -b original "$fixture/original" "$accepted"
        original="$fixture/original"
        accepted_branch=original
    fi
    jq -n --arg head "$accepted" --arg worktree "$original" --arg branch "$accepted_branch" '{worktree:$worktree,receipt:{task:"T-FIXTURE-1",agent:"FIXTURE",head:$head,closed_unix:2,branch:$branch,scopes:["src/example.rs"]}}' > "$records/$task.acceptance"
    # Retention lookup is available in a sibling; publication authority is not.
    "$SUSI_WORKFLOW_BIN" tasks completed "$task" | jq -e --arg head "$accepted" '.id == "T-FIXTURE-1" and .closed.commit == $head'
fi
claim=$(printf '%s' '{"task":"T-FIXTURE-1","agent":"FIXTURE","branch":"fixture","scopes":["src/example.rs"]}' | git hash-object -w --stdin)
git update-ref "refs/claims/$task" "$claim"
git update-ref refs/claim-agents/FIXTURE "$claim"
git push --quiet origin "$claim:refs/claims/$task" "$claim:refs/claim-agents/FIXTURE"
if [ "$existing" != absent ]; then
    receipt=$(jq -nc --arg head "$accepted" --arg branch "$existing" '{task:"T-FIXTURE-1",agent:"FIXTURE",head:$head,closed_unix:1,scopes:["src/example.rs"]} + (if $branch == "legacy" then {} else {branch:$branch} end)' | git hash-object -w --stdin)
    git update-ref "refs/closed/$task" "$receipt"
    git push --quiet origin "$receipt:refs/closed/$task"
fi
if [ "$completion" = missing ] || [ "$completion" = wrong-worktree ]; then
    if "$BASH" "$publisher" "$task" fixture > publication.out 2> publication.err; then
        echo 'unverified local completion was accepted' >&2
        exit 1
    fi
    grep -q 'publication requires a verified completion; claim retained' publication.err
    [ "$(git ls-remote origin "refs/claims/$task" | cut -f1)" = "$claim" ]
    [ "$(git ls-remote origin refs/claim-agents/FIXTURE | cut -f1)" = "$claim" ]
    [ "$(git rev-parse "refs/claims/$task")" = "$claim" ]
    [ -z "$(git ls-remote origin refs/heads/fixture "refs/closed/$task")" ]
elif [ "$existing" = wrong-branch ]; then
    if "$BASH" "$publisher" "$task" fixture > publication.out 2> publication.err; then
        echo 'mismatched receipt branch was accepted' >&2
        exit 1
    fi
    grep -q 'another branch' publication.err
    [ "$(git ls-remote origin "refs/claims/$task" | cut -f1)" = "$claim" ]
    [ "$(git ls-remote origin "refs/closed/$task" | cut -f1)" = "$receipt" ]
    [ -z "$(git ls-remote origin refs/heads/fixture)" ]
else
    "$BASH" "$publisher" "$task" fixture
    [ -z "$(git ls-remote origin "refs/claims/$task" refs/claim-agents/FIXTURE)" ]
    [ "$(git ls-remote origin refs/heads/fixture | cut -f1)" = "$(git rev-parse HEAD)" ]
    git fetch --quiet origin "refs/closed/$task"
    if [ "$existing" = legacy ]; then
        git cat-file -p FETCH_HEAD | jq -e 'has("branch") | not'
    else
        git cat-file -p FETCH_HEAD | jq -e --arg head "$accepted" '.branch == "fixture" and .head == $head and .scopes == ["src/example.rs"]'
    fi
fi
if [ "$completion" != committed ]; then
    [ ! -e ".agents/tasks/done/$task.json" ]
    ! git cat-file -e "HEAD:.agents/tasks/done/$task.json" 2>/dev/null
fi
"#;
    let mut command = Command::new(bash);
    susi_paths::console::without_window(&mut command);
    let output = command
        .args([
            "-c",
            script,
            "fixture",
            &publisher,
            existing_branch,
            completion,
        ])
        .current_dir(&fixture.0)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SUSI_HOME", home.join("susi"))
        .env("SUSI_BOARD_REMOTE", fixture.0.join("server.git"))
        .env("SUSI_BOARD_VIA_MAIN", "only")
        .env("SUSI_WORKFLOW_BIN", workflow)
        .env("SUSI_AGENT", "FIXTURE")
        .env("SUSI_TASK_REMOTE", "origin")
        .env("SUSI_QUEUE_TRANSPORT", "git")
        .env("SUSI_CLOUD_SESSION", "false")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "publication case {existing_branch}/{completion} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn deferred_publication_records_its_branch_and_releases_claim_atomically() {
    publication_case("absent", "committed");
}

#[test]
fn publication_rejects_an_existing_receipt_from_another_branch() {
    publication_case("wrong-branch", "committed");
}

#[test]
fn publication_does_not_invent_a_branch_for_a_legacy_receipt() {
    publication_case("legacy", "committed");
}

#[test]
fn publication_accepts_an_existing_receipt_for_the_same_branch() {
    publication_case("fixture", "committed");
}

#[test]
#[ignore = "requires SUSI_WORKFLOW_BIN with native tasks completed --publishable"]
fn publication_accepts_local_completion_without_growing_source_history() {
    publication_case("absent", "local");
}

#[test]
#[ignore = "requires SUSI_WORKFLOW_BIN with native tasks completed --publishable"]
fn publication_keeps_claim_when_local_history_is_missing() {
    publication_case("absent", "missing");
}

#[test]
#[ignore = "requires SUSI_WORKFLOW_BIN with native tasks completed --publishable"]
fn publication_keeps_claim_when_local_acceptance_belongs_to_a_sibling() {
    publication_case("absent", "wrong-worktree");
}

#[test]
fn fixture_drop_removes_temporary_directory_cleanly() {
    let dir = std::env::temp_dir().join(format!("susi-publication-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), "temp").unwrap();
    assert!(dir.exists());
    {
        let _guard = Fixture(dir.clone());
    }
    assert!(!dir.exists());
}
