#!/usr/bin/env bash
# Safe boundaries for the per-task agent loop. Never merge into dirty worktrees.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
common=$(git rev-parse --path-format=absolute --git-common-dir)
action=${1:-sync}
lock="$common/susi-workflow.lock"
susi_bin=${SUSI_WORKFLOW_BIN:-susi}

sync() {
    [ "$(git rev-parse --path-format=absolute --git-dir)" != "$common" ] || { echo 'Use your own linked worktree.' >&2; return 1; }
    [ "$(git symbolic-ref -q --short HEAD)" != main ] || { echo 'Use your own branch.' >&2; return 1; }
    exec 9>"$lock"
    flock 9
    [ -z "$(git status --porcelain)" ] || { echo 'Commit or stash your work before syncing.' >&2; return 1; }
    git fetch --quiet origin
    git merge --no-edit origin/main
    "$root/scripts/park-primary.sh" --sync-only
    flock -u 9
}

case "$action" in
sync) sync ;;
finish)
    task=${2:?task id required}
    "$susi_bin" tasks renew "$task"
    sync
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    # Retain ownership through publication; close records acceptance in the tree.
    if [ -f ".agents/tasks/$task.json" ]; then
        "$susi_bin" tasks close "$task"
        git add -- ".agents/tasks/$task.json" ".agents/tasks/done/$task.json"
        git commit -m "Close $task after acceptance" -m "Task: $task"
    fi
    [ -f ".agents/tasks/done/$task.json" ] || { echo 'No completed task record.' >&2; exit 1; }
    verified=$(git rev-parse HEAD)
    sync
    if [ "$verified" != "$(git rev-parse HEAD)" ]; then
        cargo fmt --all --check
        cargo clippy --workspace --all-targets --locked -- -D warnings
        cargo test --workspace --locked
    fi
    branch=$(git symbolic-ref --short HEAD)
    git push origin "HEAD:refs/heads/$branch"
    sha=$(git rev-parse HEAD)
    echo "Waiting for $sha to reach origin/main (Ctrl-C leaves work intact)."
    renew_at=$(( $(date +%s) + 900 ))
    until git fetch --quiet origin && git merge-base --is-ancestor "$sha" origin/main; do
        if [ "$(date +%s)" -ge "$renew_at" ]; then
            "$susi_bin" tasks renew "$task"
            renew_at=$(( $(date +%s) + 900 ))
        fi
        # Another agent may merge first. Integrate it, rerun the gate and
        # publish a new head so the remote merge gate can reconsider this PR.
        if ! git merge-base --is-ancestor origin/main HEAD; then
            sync
            cargo fmt --all --check
            cargo clippy --workspace --all-targets --locked -- -D warnings
            cargo test --workspace --locked
            git push origin "HEAD:refs/heads/$branch"
            sha=$(git rev-parse HEAD)
        fi
        sleep 15
    done
    sync
    "$susi_bin" tasks release "$task"
    echo 'Merged and synchronized; ready to claim the next task.'
    ;;
start-watch)
    nohup "$root/scripts/parallel-workflow.sh" watch >"$common/susi-primary-watch.log" 2>&1 </dev/null &
    ;;
watch)
    # This runs locally: hosted Actions cannot update a developer's filesystem.
    # A single watcher per clone, and a separate lock for primary tree updates.
    exec 8>"$common/susi-primary-watch.lock"
    flock -n 8 || { echo 'A primary sync watcher is already running.'; exit 0; }
    trap 'exit 0' INT TERM
    while true; do
        "$root/scripts/park-primary.sh" --sync-only
        sleep "${SUSI_SYNC_INTERVAL:-15}"
    done
    ;;
*) echo 'usage: parallel-workflow.sh sync | finish <task> | watch' >&2; exit 2 ;;
esac
