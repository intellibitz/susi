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
    if not scopes:
        return 0
    paths = git("diff", "--cached", "--name-only", "--no-renames", "-z").decode().split("\0")
    outside = [p for p in paths if p and not p.startswith(".agents/tasks/")
               and not any(p == s or p.startswith(s + "/") for s in scopes)]
    if outside:
        print("Files outside the task's reserved scopes: " + ", ".join(outside), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
