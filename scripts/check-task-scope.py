#!/usr/bin/env python3
"""Check staged files against a task's declared remote claim scopes."""
import json
import subprocess
import sys


def git(*args):
    return subprocess.check_output(["git", *args])


def main():
    task = sys.argv[1]
    try:
        claim = json.loads(git("cat-file", "-p", f"refs/claims/{task}"))
    except (subprocess.CalledProcessError, ValueError):
        # Legacy/unavailable claims are still checked by push/CI compliance.
        return 0
    owner_branch = claim.get("branch")
    branch = git("symbolic-ref", "-q", "--short", "HEAD").decode().strip()
    if owner_branch and owner_branch != branch:
        print("Task claim belongs to branch " + owner_branch, file=sys.stderr)
        return 1
    scopes = claim.get("scopes", [])
    paths = git("diff", "--cached", "--name-only", "--no-renames", "-z").decode().split("\0")
    # Task records are exempt everywhere: creating and closing a task touches
    # only .agents/tasks/, so a legacy claim that reserved nothing can still be
    # closed rather than becoming unclosable.
    code = [p for p in paths if p and not p.startswith(".agents/tasks/")]
    if not scopes:
        # An empty scope list used to skip the test entirely, which made the
        # path reservation optional in practice: claim without --scope and
        # every path was fair game, with neither this hook nor CI able to tell
        # two agents off one file. Refuse the code; the fix is a scope.
        if code:
            print(
                f"Task {task} reserves no paths — re-claim it with a scope that covers this "
                f"change (susi tasks release {task}, then susi tasks claim {task} --scope <path>): "
                + ", ".join(code),
                file=sys.stderr,
            )
            return 1
        return 0
    outside = [p for p in code if not any(p == s or p.startswith(s + "/") for s in scopes)]
    if outside:
        print("Files outside the task's reserved scopes: " + ", ".join(outside), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
