#!/usr/bin/env python3
"""Queue the work the roadmap still owes, and prove the queue is not dry.

`susi tasks roadmap` reports *coverage*, and coverage is not mastery: every one
of the 102 vectors has a linked task, and "delivered" there means every linked
task is closed. On 2026-10-02 that read as a finished roadmap while 30 vectors
did not claim delivery in their own narrative (72 start "DELIVERED:", 30 do
not), and 29 of those had nothing in the queue either — so once the eight
swarm-gap tasks closed, the queue would have been empty with the work still
outstanding. That is the gap this makes visible and closable.

A vector whose narrative does not claim delivery must be queued, or the
decision to stop must be recorded in it. `--queue` files the verification task
for each unqueued vector: prove the mastery target with a nonzero test, or
record what is missing.

    scripts/check-roadmap-queue.py            # report; exit 1 when unqueued
    scripts/check-roadmap-queue.py --queue    # file the missing verification tasks
"""
import json
import subprocess
import sys
from pathlib import Path

NAME = "roadmap queue"
ROOT = Path(__file__).resolve().parent.parent


def git(*args):
    out = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    return out.stdout if out.returncode == 0 else ""


def tasks(kind):
    """Task records in this checkout: ('open' | 'done') -> [record].

    Read from the working tree, not HEAD: `--queue` files the verification
    tasks and they stay untracked until the commit that carries them, so a
    HEAD-based read would report them missing and mint them twice.
    """
    out = []
    directory = ROOT / ".agents/tasks" / kind
    if not directory.is_dir():
        return out
    for path in sorted(directory.glob("*.json")):
        try:
            out.append(json.loads(path.read_text()))
        except ValueError:
            continue
    return out


def claims_delivery(vector):
    """The vector's own narrative says the capability is delivered."""
    return (vector.get("progress") or "").strip().startswith("DELIVERED")


def filter_name(vector):
    return vector["id"].lower().replace("-", "_")


def mint(vector):
    accept = f"cargo nextest run --locked -E test({filter_name(vector)}_mastery)"
    goal = (
        f"{vector['id']} ({vector['priority']}) claims mastery of: "
        f"{vector.get('mastery_target', '')}. Its own narrative does not claim delivery: "
        f"{(vector.get('progress') or '').strip()[:400]}. Prove or refute it: add a nonzero test "
        f"named {filter_name(vector)}_mastery_* that exercises the capability on its production "
        f"path, and record exactly which part is missing when it cannot be proven. Coverage is not "
        f"mastery: this closes only when the capability is verified, and a linked closed task is "
        f"not evidence."
    )
    args = [
        "susi", "tasks", "add", f"Verify mastery: {vector['vector']}",
        "--goal", goal,
        "--accept", accept,
        "--size", "l" if vector["priority"] == "P0" else "m",
        "--roadmap", vector["id"],
        "--agent", "DEEPSEEK",
    ]
    out = subprocess.run(args, cwd=ROOT, capture_output=True, text=True)
    line = (out.stdout or out.stderr).strip().splitlines()
    return (out.returncode == 0, line[-1] if line else "")


def main():
    queue = "--queue" in sys.argv
    roadmap = json.loads((ROOT / ".agents/roadmap.json").read_text())["vectors"]
    open_tasks = tasks("")
    done_tasks = tasks("done")
    linked_open = {t.get("roadmap") for t in open_tasks}
    linked_any = linked_open | {t.get("roadmap") for t in done_tasks}

    covered = [v for v in roadmap if v["id"] in linked_any]
    claimed = [v for v in roadmap if claims_delivery(v)]
    unverified = [v for v in roadmap if not claims_delivery(v)]
    unqueued = [v for v in unverified if v["id"] not in linked_open]

    print(f"{NAME}: {len(roadmap)} vectors — coverage {len(covered)} linked to a task, "
          f"mastery {len(claimed)} claim delivery, {len(unverified)} do not "
          f"({len(unqueued)} of those unqueued)")

    if not unqueued:
        print(f"✅ {NAME}: every vector either claims delivery or has work in the queue")
        return 0

    if not queue:
        for v in sorted(unqueued, key=lambda v: (v["priority"], v["id"])):
            print(f"❌ {NAME}: {v['id']} ({v['priority']}) {v['vector']} — not delivered by its own "
                  f"narrative and nothing queued: susi tasks add \"Verify mastery: {v['vector']}\" "
                  f"--accept \"cargo nextest run --locked -E test({filter_name(v)}_mastery)\" "
                  f"--roadmap {v['id']}")
        print(f"\n{len(unqueued)} vector(s) would be left with no work once the queue empties; "
              f"re-run with --queue to file them")
        return 1

    filed = 0
    for v in sorted(unqueued, key=lambda v: (v["priority"], v["id"])):
        ok, message = mint(v)
        if ok:
            filed += 1
            print(f"  filed {message}")
        else:
            print(f"❌ {NAME}: could not file {v['id']}: {message}")
    print(f"filed {filed} verification task(s); re-run without --queue to confirm")
    return 0 if filed == len(unqueued) else 1


if __name__ == "__main__":
    sys.exit(main())
