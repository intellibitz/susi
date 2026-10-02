#!/usr/bin/env python3
"""Is this roadmap vector's mastery verdict recorded, and is it complete?

A verification task asks an agent to prove a vector's `mastery_target`. Many of
these vectors will turn out **not** to be delivered — that is what the
verification is for — and a task whose acceptance is "a test proves the
capability" cannot be closed on that finding: the agent would hold a dead claim
with the work done and the queue jammed. So the *result* is what closes it, and
the result is a record, not a passed test.

One file per vector, so two agents recording two vectors never touch the same
file (hand-resolved `roadmap.json` conflicts are what stranded the swarm-gap
branch for a day).

    .agents/roadmap-verdicts/<VC-id>.json
    {
      "vector": "VC-201-071",
      "verdict": "delivered",            # or "not-delivered"
      "evidence": "vc_201_071_mastery_policy_decision",   # delivered: what proves it
      "missing": "",                      # not-delivered: what is absent
      "tracked_by": [],                   # not-delivered: task ids tracking the work
      "recorded_by": "DEEPSEEK",
      "recorded_unix": 1790958000
    }

    scripts/check-roadmap-verdict.py VC-201-071      # 0 when the verdict is complete
    scripts/check-roadmap-verdict.py --self-test     # pin all four cases
"""
import json
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERDICTS = ROOT / ".agents/roadmap-verdicts"


def validate(record, vector):
    """What is missing from this verdict record, if anything."""
    problems = []
    if record.get("vector") != vector:
        problems.append(f"vector must be {vector!r}")
    verdict = record.get("verdict")
    if verdict not in ("delivered", "not-delivered"):
        problems.append("verdict must be 'delivered' or 'not-delivered'")
    elif verdict == "delivered":
        if not str(record.get("evidence") or "").strip():
            problems.append("a delivered verdict needs evidence: the test or record that proves it")
    else:
        if not str(record.get("missing") or "").strip():
            problems.append("a not-delivered verdict needs 'missing': what the vector still lacks")
        tracked = record.get("tracked_by") or []
        if not tracked:
            problems.append("a not-delivered verdict needs tracked_by: the task id(s) that will close it")
        else:
            queue = {path.stem for path in (ROOT / ".agents/tasks").glob("*.json")}
            queue |= {path.stem for path in (ROOT / ".agents/tasks/done").glob("*.json")}
            for task in tracked:
                if task not in queue:
                    problems.append(f"tracked_by names {task}, which is not in the queue")
    if not str(record.get("recorded_by") or "").strip():
        problems.append("recorded_by is required")
    return problems


def check(vector, base=None):
    path = (base or VERDICTS) / f"{vector}.json"
    if not path.is_file():
        print(f"❌ no verdict recorded for {vector}: write {path.relative_to(ROOT) if base is None else path}")
        print("   delivered → evidence proving it; not-delivered → what is missing and the task(s) tracking it")
        return 1
    try:
        record = json.loads(path.read_text())
    except ValueError as err:
        print(f"❌ {path.name} is not valid JSON: {err}")
        return 1
    problems = validate(record, vector)
    if problems:
        print(f"❌ {path.name} is incomplete:")
        for problem in problems:
            print(f"   - {problem}")
        return 1
    verdict = record["verdict"]
    if verdict == "delivered":
        print(f"✅ {vector} delivered — evidence: {record['evidence']}")
    else:
        print(f"✅ {vector} not delivered, recorded and tracked by {', '.join(record['tracked_by'])}")
    return 0


def self_test():
    """Prove the four cases, without touching the real queue."""
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        good_delivered = {
            "vector": "VC-201-001", "verdict": "delivered",
            "evidence": "vc_201_001_mastery_corpus", "recorded_by": "TEST", "recorded_unix": 1,
        }
        good_refuted = {
            "vector": "VC-201-002", "verdict": "not-delivered", "missing": "no egress check",
            "tracked_by": [next((ROOT / ".agents/tasks").glob("T-*.json")).stem],
            "recorded_by": "TEST", "recorded_unix": 1,
        }
        cases = [
            ("delivered, complete", good_delivered, 0),
            ("refuted, complete", good_refuted, 0),
            ("delivered with no evidence", {**good_delivered, "evidence": ""}, 1),
            ("refuted with nothing tracking it", {**good_refuted, "tracked_by": []}, 1),
        ]
        failures = 0
        for name, record, expected in cases:
            vector = record["vector"]
            (base / f"{vector}.json").write_text(json.dumps(record))
            got = check(vector, base=base)
            status = "ok" if got == expected else "WRONG"
            if got != expected:
                failures += 1
            print(f"  self-test: {name} → exit {got} (expected {expected}) {status}")
            (base / f"{vector}.json").unlink()
        if failures:
            print(f"self-test FAILED: {failures} case(s) wrong")
            return 1
        print("PASS: verdict records close a verification task on evidence or on a refutation")
        return 0


def main():
    argv = sys.argv[1:]
    if not argv:
        print("usage: check-roadmap-verdict.py <VC-id> | --self-test", file=sys.stderr)
        return 2
    if argv[0] == "--self-test":
        return self_test()
    return check(argv[0])


if __name__ == "__main__":
    sys.exit(main())
