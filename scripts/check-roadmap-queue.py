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


def verdict_records():
    """The recorded verdict for each vector, by id (see check-roadmap-verdict.py)."""
    records = {}
    directory = ROOT / ".agents/roadmap-verdicts"
    if not directory.is_dir():
        return records
    for path in sorted(directory.glob("*.json")):
        try:
            record = json.loads(path.read_text())
        except ValueError:
            continue
        if record.get("verdict") in ("delivered", "not-delivered"):
            records[record.get("vector") or path.stem] = record
    return records


def consistency_problems(roadmap, records):
    """Narrative claims that contradict the verdict recorded for that vector.

    Coverage is not completion in both directions: a delivered verdict that the
    narrative still calls unbuilt, and a refutation the narrative still presents
    as delivered, are each a roadmap a human cannot trust.
    """
    problems = []
    for vector in roadmap:
        record = records.get(vector["id"])
        if not record:
            continue
        verdict = record.get("verdict")
        claims = claims_delivery(vector)
        if verdict == "delivered" and not claims:
            problems.append(
                f"{vector['id']} has a delivered verdict on record, but its narrative does "
                f"not claim delivery: {(vector.get('progress') or '').strip()[:80]}"
            )
        elif verdict == "not-delivered" and claims:
            problems.append(
                f"{vector['id']} is refuted on record, but its narrative still claims "
                f"delivery: {(vector.get('progress') or '').strip()[:80]}"
            )
    return problems


def recorded_verdicts():
    """Vectors whose mastery verdict is on file (see check-roadmap-verdict.py).

    A recorded verdict is a resolution: the capability was proven, or refuted
    with the follow-up tracked. Counting only *open* tasks would report a
    refuted vector as unqueued forever.
    """
    return set(verdict_records())


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


def self_test():
    """Pin the four narrative/verdict combinations."""
    cases = [
        ("delivered, narrative agrees", {"progress": "DELIVERED: proof"}, "delivered", 0),
        ("delivered, narrative disagrees", {"progress": "PARTIAL: half"}, "delivered", 1),
        ("refuted, narrative agrees", {"progress": "PARTIAL: half"}, "not-delivered", 0),
        ("refuted, narrative disagrees", {"progress": "DELIVERED: claimed"}, "not-delivered", 1),
    ]
    failures = 0
    for name, vector, verdict, expected in cases:
        vector = {"id": "VC-0-1", **vector}
        problems = consistency_problems([vector], {"VC-0-1": {"verdict": verdict}})
        got = 1 if problems else 0
        ok = got == expected
        failures += 0 if ok else 1
        print(f"  self-test: {name} → {got} problem(s) (expected {expected}) {'ok' if ok else 'WRONG'}")
    # No verdict on record is not a contradiction: nothing was claimed about it.
    extra = consistency_problems([{"id": "VC-0-1", "progress": "PARTIAL"}], {})
    if extra:
        failures += 1
        print("  self-test: no verdict recorded → WRONG (must not be a contradiction)")
    if failures:
        print(f"self-test FAILED: {failures} case(s) wrong")
        return 1
    print("PASS: a narrative that contradicts its recorded verdict is a failure")
    return 0


def main():
    if "--self-test" in sys.argv:
        return self_test()
    # CI runs this form: a narrative that contradicts a recorded verdict is a
    # false claim about the repo, which must not merge. "A vector still has no
    # work queued" is a backlog state, visible at every sync, and does not block.
    if "--consistency-only" in sys.argv:
        roadmap = json.loads((ROOT / ".agents/roadmap.json").read_text())["vectors"]
        problems = consistency_problems(roadmap, verdict_records())
        for problem in problems:
            print(f"❌ {NAME}: {problem}")
        if problems:
            return 1
        print(f"✅ {NAME}: every recorded verdict agrees with its vector's narrative")
        return 0
    queue = "--queue" in sys.argv
    roadmap = json.loads((ROOT / ".agents/roadmap.json").read_text())["vectors"]
    open_tasks = tasks("")
    done_tasks = tasks("done")
    linked_open = {t.get("roadmap") for t in open_tasks}
    linked_any = linked_open | {t.get("roadmap") for t in done_tasks}

    covered = [v for v in roadmap if v["id"] in linked_any]
    claimed = [v for v in roadmap if claims_delivery(v)]
    unverified = [v for v in roadmap if not claims_delivery(v)]
    problems = consistency_problems(roadmap, verdict_records())
    for problem in problems:
        print(f"❌ {NAME}: {problem}")
    if problems:
        print(f"\n{len(problems)} vector(s) contradict their recorded verdict; fix the narrative "
              f"(the verdict is the evidence, the narrative is the claim)")
        return 1

    recorded = recorded_verdicts()
    unqueued = [
        v for v in unverified if v["id"] not in linked_open and v["id"] not in recorded
    ]

    print(f"{NAME}: {len(roadmap)} vectors — coverage {len(covered)} linked to a task, "
          f"mastery {len(claimed)} claim delivery, {len(unverified)} do not "
          f"({len(recorded)} verified or refuted on record, {len(unqueued)} unqueued)")

    if not unqueued:
        print(f"✅ {NAME}: every vector claims delivery, is verified or refuted on record, "
              f"or has work in the queue")
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
