#!/usr/bin/env python3
"""Board hygiene: merged remote branches and claim refs nobody can act on.

Two kinds of debris accumulate while parallel agents work, and neither breaks
the loop — which is exactly why nothing ever removed them:

* **Merged feature branches on the remote.** Every task leaves its branch
  behind; `scripts/prune-worktrees.sh` reclaims the *checkout* and deliberately
  leaves the ref alone, so the remote keeps growing. Anything merged into
  `origin/main` is safe to delete: its commits are in main, and a branch that
  merges later is recreated by the push that needs it.
* **Dead claim refs.** A claim whose lease lapsed and whose task is already
  closed on `origin/main` (or no longer exists) can be acted on by nobody: the
  task will not be worked again, and the ref only makes the board look busier
  than it is.

An *expired claim on a still-open task* is a different thing and not debris: it
is a crashed or abandoned worker's lapse, takeable by design, and the fix is to
claim the task, not to delete the record. It is reported, and never fails.

Usage:
    scripts/check-board-hygiene.py            # report; exit 1 when dirty
    scripts/check-board-hygiene.py --apply    # remove what it reported
"""
import json
import subprocess
import sys
import time

NAME = "board hygiene"
TMP_NAMESPACE = "refs/board-hygiene/claims"


def git(*args, check=True):
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, check=check
    ).stdout


def ok(*args):
    return subprocess.run(["git", *args], capture_output=True).returncode == 0


def fetch_claims(remote):
    """One fetch for every claim blob, into a namespace this script removes."""
    git("fetch", "--quiet", "--prune", remote,
        f"+refs/claims/*:{TMP_NAMESPACE}/*", check=False)
    claims = []
    for line in git("for-each-ref", "--format=%(refname)", TMP_NAMESPACE).split():
        task = line[len(TMP_NAMESPACE) + 1:]
        try:
            claims.append((task, json.loads(git("cat-file", "-p", line, check=False))))
        except ValueError:
            claims.append((task, {}))
    return claims


def drop_tmp():
    for line in git("for-each-ref", "--format=%(refname)", TMP_NAMESPACE).split():
        git("update-ref", "-d", line, check=False)


def task_state(remote, task):
    """open / closed / missing, judged on the remote's main, not this tree."""
    base = f"refs/remotes/{remote}/main"
    if ok("cat-file", "-e", f"{base}:.agents/tasks/{task}.json"):
        return "open"
    if ok("cat-file", "-e", f"{base}:.agents/tasks/done/{task}.json"):
        return "closed"
    return "missing"


def merged_branches(remote):
    merged = []
    prefix = f"refs/remotes/{remote}/"
    # Full refnames, not `:short`: the `origin/HEAD` symref shortens to `HEAD`,
    # which escaped the prefix strip and was reported as a branch with an empty
    # name — for which `git push --delete ""` would have been run.
    refs = git("for-each-ref", "--format=%(refname) %(objectname)",
               f"refs/remotes/{remote}/")
    for line in refs.splitlines():
        name, _, sha = line.partition(" ")
        short = name[len(prefix):] if name.startswith(prefix) else ""
        if not sha or short in ("", "main", "HEAD"):
            continue
        if ok("merge-base", "--is-ancestor", sha, f"{remote}/main"):
            merged.append((short, sha))
    return merged


def main():
    apply = "--apply" in sys.argv
    remote = "origin"
    now = int(time.time())
    git("fetch", "--quiet", "--prune", remote)
    claims = fetch_claims(remote)
    merged = merged_branches(remote)

    dead, lapsed_open = [], []
    for task, claim in claims:
        if int(claim.get("lease_until_unix") or 0) > now:
            continue
        row = (task, claim.get("agent", "?"), task_state(remote, task))
        (lapsed_open if row[2] == "open" else dead).append(row)

    for task, agent, _ in lapsed_open:
        print(f"⚠️  {NAME}: {task} ({agent}) has a lapsed claim on a task that is "
              f"still open — claimable, not debris: susi tasks claim {task}")
    for task, agent, state in dead:
        print(f"❌ {NAME}: claim ref {task} ({agent}, task {state}) is dead — "
              f"git push {remote} :refs/claims/{task}")
    for short, sha in merged:
        print(f"❌ {NAME}: {short} ({sha[:8]}) is merged into {remote}/main — "
              f"git push {remote} --delete {short}")

    if not dead and not merged:
        print(f"✅ {NAME}: no merged branches, no dead claim refs "
              f"({len(claims)} claim ref(s) read)")
        drop_tmp()
        return 0

    if not apply:
        print(f"\n{len(dead)} dead claim ref(s), {len(merged)} merged branch(es); "
              f"re-run with --apply to remove them")
        drop_tmp()
        return 1

    removed_agents = {agent for _, agent, _ in dead}
    surviving_agents = {
        claim.get("agent")
        for task, claim in claims
        if task not in {t for t, _, _ in dead}
    }
    for task, _, _ in dead:
        git("push", "--quiet", remote, f":refs/claims/{task}")
    # An agent pointer rides with the claims it names: drop it only when that
    # agent has nothing left on the board.
    for agent in removed_agents - surviving_agents:
        git("push", "--quiet", remote, f":refs/claim-agents/{agent}", check=False)
    for short, _ in merged:
        git("push", "--quiet", remote, "--delete", short)

    drop_tmp()
    print(f"removed {len(dead)} dead claim ref(s) and {len(merged)} merged branch(es)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
