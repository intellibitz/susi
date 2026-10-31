#!/usr/bin/env python3
"""Publish accepted code and release ownership in one compare-and-swap push.

Exit 75 requests the existing separate queue path (cloud relay, another queue
remote, or an already released claim). Other failures never fall back or release.
"""
import io
import json
import os
from pathlib import Path
import re
import string
import subprocess
import sys
import time

sys.dont_write_bytecode = True
from workflow_git import git_executable


def publish(task, branch):
    if not re.fullmatch(r"T-[A-Z0-9]+-[0-9]+", task) or branch in ("main", "master", ""):
        raise ValueError("publication requires a task and its feature branch")
    if os.environ.get("SUSI_TASK_REMOTE", "origin") != "origin":
        return 75
    if os.environ.get("SUSI_QUEUE_TRANSPORT", "").strip().lower() == "relay":
        return 75
    binary, _ = git_executable()

    def git(*args, input=None):
        return subprocess.check_output([binary, *args], input=input)

    config = subprocess.run(
        [binary, "config", "--null", "--get-regexp", r"^susi\.(agent|cloud)$"],
        stdout=subprocess.PIPE, check=False,
    )
    if config.returncode not in (0, 1):
        raise ValueError("cannot read the workflow configuration")
    values = {}
    for entry in config.stdout.split(b"\0"):
        if entry:
            key, separator, value = entry.partition(b"\n")
            values[key.decode()] = value.decode() if separator else "true"
    explicit = os.environ.get("SUSI_CLOUD_SESSION", "").lower()
    cloud = values.get("susi.cloud", "false").lower() in ("true", "yes", "on", "1")
    if explicit in ("true", "yes", "on", "1"):
        cloud = True
    elif explicit in ("false", "no", "off", "0"):
        cloud = False
    if cloud:
        return 75
    raw_agent = os.environ.get("SUSI_AGENT", values.get("susi.agent", ""))
    agent = "".join(c for c in raw_agent if c in string.ascii_letters + string.digits).upper()
    if not agent:
        raise ValueError("publication requires this worktree's identity")
    here = git("symbolic-ref", "--quiet", "--short", "HEAD").decode().strip()
    if here != branch:
        raise ValueError("the branch changed before publication")
    head = git("rev-parse", "HEAD").decode().strip()
    claim_ref = f"refs/claims/{task}"
    agent_ref = f"refs/claim-agents/{agent}"
    receipt_ref = f"refs/closed/{task}"
    names = [claim_ref, agent_ref, receipt_ref, f"{head}:.agents/tasks/done/{task}.json"]
    stream = io.BytesIO(git("cat-file", "--batch", input=("\n".join(names) + "\n").encode()))
    records = []
    for name in names:
        header = stream.readline().split()
        if len(header) == 2 and header[1] == b"missing":
            records.append(None)
            continue
        if len(header) != 3 or header[1] != b"blob":
            raise ValueError(f"invalid workflow blob: {name}")
        body = stream.read(int(header[2]))
        if stream.read(1) != b"\n":
            raise ValueError("truncated workflow snapshot")
        record = json.loads(body)
        if not isinstance(record, dict):
            raise ValueError(f"invalid workflow record: {name}")
        records.append((header[0].decode(), record))
    claim, index, receipt, done = records
    if claim is None:
        return 75
    for record in (claim, index):
        if record is not None:
            owner = record[1]
            if (owner.get("task") != task or owner.get("agent") != agent
                    or owner.get("branch") not in (None, branch)):
                raise ValueError("ownership changed before publication; claim retained")
    if receipt is None or done is None:
        raise ValueError("publication requires the accepted receipt and committed completion")
    accepted = receipt[1]
    closed = done[1].get("closed", {})
    if not isinstance(closed, dict):
        raise ValueError("the committed completion has no closure record")
    if (accepted.get("task") != task or accepted.get("agent") != agent
            or done[1].get("id") != task or closed.get("by") != agent
            or closed.get("commit") != accepted.get("head")):
        raise ValueError("the accepted receipt and completion do not agree")
    if not isinstance(accepted.get("head"), str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", accepted["head"]):
        raise ValueError("the accepted head is not a commit object ID")
    git("merge-base", "--is-ancestor", accepted["head"], head)
    publication = accepted.get("publication", {})
    revision = publication.get("revision", 0) if isinstance(publication, dict) else 0
    if not isinstance(revision, int) or not 0 <= revision < 2**63:
        raise ValueError("invalid publication revision")
    # A new revision makes this a real ref update, even when republishing the
    # same head. Git skips no-op updates and would not check their leases.
    accepted["publication"] = {"revision": revision + 1, "head": head, "at_unix": int(time.time())}
    payload = json.dumps(accepted, sort_keys=True, separators=(",", ":")).encode()
    new_receipt = git("hash-object", "-w", "--stdin", input=payload).decode().strip()
    leases = [(claim_ref, claim[0]), (receipt_ref, receipt[0])]
    updates = [(claim_ref, None), (receipt_ref, new_receipt)]
    if index is not None:
        leases.append((agent_ref, index[0]))
        updates.append((agent_ref, None))
    command = [binary, "push", "--atomic"]
    command.extend(f"--force-with-lease={name}:{old}" for name, old in leases)
    command.extend(["origin", f"{head}:refs/heads/{branch}"])
    command.extend(f"{value or ''}:{name}" for name, value in updates)
    result = subprocess.run(command, check=False)
    if result.returncode:
        return 1
    # The remote is authoritative. A newer concurrent fetch must never be
    # overwritten just to tidy the local mirror; the next boundary refreshes it.
    old = dict(leases)
    mirror = "start\n" + "".join(
        f"update {name} {value} {old[name]}\n" if value else f"delete {name} {old[name]}\n"
        for name, value in updates
    ) + "prepare\ncommit\n"
    subprocess.run([binary, "update-ref", "--no-deref", "--stdin"], input=mirror.encode(),
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    if git("rev-parse", "HEAD").decode().strip() != head:
        raise ValueError(f"published {head}, but HEAD changed during publication")
    print("workflow: branch publication and claim release committed atomically", file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3:
            raise ValueError("usage: publish-task-branch.py <task> <branch>")
        sys.exit(publish(*sys.argv[1:]))
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        print(f"workflow: {error}", file=sys.stderr)
        sys.exit(1)
