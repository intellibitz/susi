//! Exercise the actual atomic publisher against a disposable bare Git remote.

use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn publication_case(existing_branch: &str) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("susi-publication-{}-{unique}", std::process::id())),
    );
    std::fs::create_dir_all(&fixture.0).unwrap();
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
git init --bare --quiet server.git
git init --quiet -b fixture branch
cd branch
git config user.name fixture
git config user.email fixture@localhost
git config commit.gpgsign false
git config core.hooksPath nonexistent-hooks
git config gc.auto 0
git config maintenance.auto false
git remote add origin ../server.git
git commit --quiet --allow-empty -m accepted
accepted=$(git rev-parse HEAD)
task=T-FIXTURE-1
mkdir -p .agents/tasks/done
jq -n --arg head "$accepted" '{id:"T-FIXTURE-1",closed:{by:"FIXTURE",commit:$head,at_unix:1}}' > ".agents/tasks/done/$task.json"
git add .agents
git commit --quiet -m closure
claim=$(printf '%s' '{"task":"T-FIXTURE-1","agent":"FIXTURE","branch":"fixture","scopes":["src/example.rs"]}' | git hash-object -w --stdin)
git update-ref "refs/claims/$task" "$claim"
git update-ref refs/claim-agents/FIXTURE "$claim"
git push --quiet origin "$claim:refs/claims/$task" "$claim:refs/claim-agents/FIXTURE"
if [ "$existing" != absent ]; then
    receipt=$(jq -nc --arg head "$accepted" --arg branch "$existing" '{task:"T-FIXTURE-1",agent:"FIXTURE",head:$head,closed_unix:1} + (if $branch == "legacy" then {} else {branch:$branch} end)' | git hash-object -w --stdin)
    git update-ref "refs/closed/$task" "$receipt"
    git push --quiet origin "$receipt:refs/closed/$task"
fi
if [ "$existing" = wrong-branch ]; then
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
"#;
    let mut command = Command::new(bash);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command
        .args(["-c", script, "fixture", &publisher, existing_branch])
        .current_dir(&fixture.0)
        .env("SUSI_AGENT", "FIXTURE")
        .env("SUSI_TASK_REMOTE", "origin")
        .env("SUSI_QUEUE_TRANSPORT", "git")
        .env("SUSI_CLOUD_SESSION", "false")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "publication case {existing_branch} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn deferred_publication_records_its_branch_and_releases_claim_atomically() {
    publication_case("absent");
}

#[test]
fn publication_rejects_an_existing_receipt_from_another_branch() {
    publication_case("wrong-branch");
}

#[test]
fn publication_does_not_invent_a_branch_for_a_legacy_receipt() {
    publication_case("legacy");
}
