#!/usr/bin/env python3
"""Can the waiting agents start, in parallel, right now?

Every guarantee the workflow claims — one claim per task, one branch per claim,
disjoint path reservations — is worth nothing if a worker cannot get as far as
claiming: `susi tasks claim` refuses from a worktree behind `origin/main`, the
hooks must be installed where the worker is, and the queue must actually hold a
task that is dependency-ready and unclaimed. This checks all of that per agent,
and reports unlanded work it must not lose.

Usage:
    scripts/check-swarm-readiness.py [--agent NAME ...]

Exit 0 when every agent in the roster is ready and at least one task is
claimable; 1 otherwise, with the command that fixes each line.
"""
import json
import subprocess
import sys
from pathlib import Path

NAME = "swarm readiness"
ROSTER = ["cursor", "claude", "devin", "codex", "antigravity"]
HOOKS = ["commit-msg", "pre-commit", "pre-push", "workflow-guard"]


def git(*args, check=False):
    out = subprocess.run(["git", *args], capture_output=True, text=True)
    return out.stdout if out.returncode == 0 else (None if check else "")


def worktrees():
    """(path, branch) for every worktree of this clone."""
    rows, path = [], None
    for line in git("worktree", "list", "--porcelain").splitlines():
        if line.startswith("worktree "):
            path = line[len("worktree ") :]
        elif line.startswith("branch ") and path:
            rows.append((Path(path), line[len("branch ") :].replace("refs/heads/", "")))
            path = None
    return rows


def agent_of(path):
    value = git("-C", str(path), "config", "--worktree", "--get", "susi.agent")
    return (value or "").strip().upper()


def worker_tokens():
    """The worktree-scoped identity of every worktree, keyed by path."""
    return {path: agent_of(path) for path, _ in worktrees()}


def shared_tokens(tokens):
    """Tokens carried by more than one worktree — the collision itself.

    `scripts/susi-worktree.sh` names a worker `<agent>-<date>-<time>`, so the
    token is that name uppercased. Two worktrees sharing one means one
    `refs/claim-agents` entry, one task-id namespace, and the ability to renew,
    close or release each other's claims (Mandate 50).
    """
    seen = {}
    for path, token in tokens.items():
        if not token:
            continue
        seen.setdefault(token, []).append(path)
    return {token: paths for token, paths in seen.items() if len(paths) > 1}


def tokenless(tokens):
    """Worktrees carrying no identity: they inherit the clone-wide fallback."""
    return [path for path, token in tokens.items() if not token]


def ready_worktree(agent, tokens):
    """The worktree carrying this agent's identity, or the reason there is none."""
    want = agent.upper()
    for path, branch in worktrees():
        token = tokens.get(path, "")
        if token and token.startswith(want):
            return path, branch, None
    return None, None, (
        f"no worktree carries a susi.agent starting {want} — "
        f"scripts/susi-worktree.sh {agent}-$(date +%Y%m%d-%H%M%S)"
    )


def check_worktree(agent, path, branch):
    """Everything a worker needs before its first claim."""
    problems = []
    behind = git("-C", str(path), "rev-list", "--count", "HEAD..origin/main")
    try:
        if int(behind or 0) > 0:
            problems.append(f"{behind} commit(s) behind origin/main — git fetch origin && git merge origin/main")
    except ValueError:
        problems.append("cannot count its distance from origin/main — git fetch origin")
    hooks = (git("-C", str(path), "config", "--get", "core.hooksPath") or "").strip()
    resolved = Path(hooks) if hooks.startswith("/") else path / hooks
    missing = [h for h in HOOKS if not (resolved / h).is_file()]
    if hooks != ".githooks" and not hooks:
        problems.append("core.hooksPath unset — scripts/setup-dev.sh")
    elif missing:
        problems.append(f"hooks missing or not executable: {', '.join(missing)} — scripts/setup-dev.sh")
    if (git("-C", str(path), "status", "--porcelain") or "").strip():
        problems.append("worktree is dirty — commit or stash before the loop starts")
    return problems


def origin_tasks():
    """Task records on the remote's main: (open, closed) ids."""
    open_ids, closed_ids = set(), set()
    listing = git("ls-tree", "--name-only", "origin/main:.agents/tasks/") or ""
    for name in listing.split():
        if name.endswith(".json"):
            open_ids.add(name[: -len(".json")])
    for name in (git("ls-tree", "--name-only", "origin/main:.agents/tasks/done/") or "").split():
        if name.endswith(".json"):
            closed_ids.add(name[: -len(".json")])
    return open_ids, closed_ids


def claimable(open_ids, closed_ids):
    """Open tasks whose dependencies are closed and which nobody holds."""
    live = set()
    for line in (git("ls-remote", "origin", "refs/claims/*") or "").splitlines():
        parts = line.split()
        if len(parts) == 2:
            live.add(parts[1].replace("refs/claims/", ""))
    ready = []
    for tid in sorted(open_ids):
        body = git("show", f"origin/main:.agents/tasks/{tid}.json")
        try:
            task = json.loads(body)
        except ValueError:
            continue
        deps = task.get("deps") or []
        if tid in live:
            continue
        if any(dep not in closed_ids for dep in deps):
            continue
        ready.append((tid, task.get("size", "?"), deps))
    return ready


def unlanded(fresh_paths):
    """Worktrees holding work that is not on origin/main — must not be lost."""
    rows = []
    for path, branch in worktrees():
        if path in fresh_paths:
            continue
        dirty = bool((git("-C", str(path), "status", "--porcelain") or "").strip())
        ahead = git("-C", str(path), "rev-list", "--count", "origin/main..HEAD") or "0"
        try:
            ahead_n = int(ahead)
        except ValueError:
            ahead_n = 0
        if dirty or ahead_n > 0:
            rows.append((path, branch, ahead_n, dirty))
    return rows


def main():
    argv = sys.argv[1:]
    roster = ROSTER
    if "--agent" in argv:
        roster = [argv[i + 1] for i, a in enumerate(argv) if a == "--agent" and i + 1 < len(argv)]
    git("fetch", "--quiet", "origin")
    open_ids, closed_ids = origin_tasks()
    ready = claimable(open_ids, closed_ids)
    tokens = worker_tokens()

    failures = 0
    # A token shared by two worktrees is a collision even if both look ready:
    # they would share one refs/claim-agents entry, one task-id namespace, and
    # could renew, close or release each other's claims.
    for token, paths in sorted(shared_tokens(tokens).items()):
        failures += 1
        print(f"❌ {NAME}: {token} is the identity of {len(paths)} worktrees "
              f"({', '.join(str(p) for p in paths)}) — one token per worker: "
              f"re-provision the extras with a distinct name")

    # Not a failure: retired trees are allowed to carry nothing, but a worker
    # that claims from one silently shares the clone-wide fallback token with
    # every other such tree.
    silent = tokenless(tokens)
    if silent:
        print(f"⚠️  {NAME}: {len(silent)} worktree(s) carry no susi.agent and would share the "
              f"clone-wide token if they claimed — give each worker its own: "
              f"scripts/susi-worktree.sh <agent>-$(date +%Y%m%d-%H%M%S) "
              f"({', '.join(str(p) for p in silent[:3])}{', …' if len(silent) > 3 else ''})")

    fresh_paths = []
    for agent in roster:
        path, branch, missing = ready_worktree(agent, tokens)
        if missing:
            print(f"❌ {NAME}: {agent}: {missing}")
            failures += 1
            continue
        fresh_paths.append(path)
        problems = check_worktree(agent, path, branch)
        if problems:
            failures += 1
            for problem in problems:
                print(f"❌ {NAME}: {agent} ({path}): {problem}")
        else:
            print(f"✅ {NAME}: {agent} ready in {path} (branch {branch}, {tokens.get(path)})")

    if not ready:
        print(f"❌ {NAME}: no claimable task — every open task is claimed or dependency-blocked "
              f"({len(open_ids)} open, {len(closed_ids)} closed); add one: susi tasks add \"…\" --accept \"cargo test …\"")
        failures += 1
    else:
        print(f"✅ {NAME}: {len(ready)} claimable task(s) for {len(roster)} agent(s): "
              + ", ".join(f"{tid} ({size})" for tid, size, _ in ready))
        if len(ready) < len(roster):
            print(f"⚠️  {NAME}: fewer claimable tasks than agents — the rest will wait rather than collide")

    for path, branch, ahead_n, dirty in unlanded(fresh_paths):
        held = []
        if ahead_n:
            held.append(f"{ahead_n} unpushed commit(s)")
        if dirty:
            held.append("uncommitted changes")
        print(f"⚠️  {NAME}: {path} ({branch}) holds {' and '.join(held)} — not on origin/main; "
              f"land it as a task or discard it deliberately")

    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
