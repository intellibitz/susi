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
import pathlib
import re
import subprocess
from pathlib import PurePath
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERDICTS = ROOT / ".agents/roadmap-verdicts"


def cited_path(evidence):
    """The source path an evidence string cites: `test (path)`, or a bare path.

    Agents write evidence as `<test identifier> (<path>)`, so the first
    whitespace token is the *test*, not the file - reading it as a path silently
    skipped every reachability check.
    """
    match = re.search(r"\(([^)]+\.rs)\)", evidence)
    if match:
        return match.group(1).strip()
    first = evidence.split()[0] if evidence.split() else evidence
    return first if first.endswith(".rs") else ""


def evidence_exists(evidence, root=None):
    """Does this evidence name something real in the checkout?

    The checkout, not the index: the loop closes a task *before* it commits, so
    at the moment this runs the test file and the verdict are both untracked, and
    a search over tracked files would reject every honest delivery. Verdict
    records, task files and this checker are excluded - a task describing the
    test it expects is prose, not evidence.
    """
    root = root or ROOT
    cited = cited_path(evidence)
    if cited:
        return (root / cited).is_file()
    found = subprocess.run(
        ["grep", "-rIl", "--exclude-dir=.git", "--exclude-dir=target", "-F", evidence, "."],
        cwd=root, capture_output=True, text=True,
    )
    for line in found.stdout.splitlines():
        rel = line.lstrip("./")
        if rel.startswith((".agents/roadmap-verdicts/", ".agents/tasks/")) or rel == "scripts/check-roadmap-verdict.py":
            continue
        return True
    return False


def declared_in(text, stem, filename):
    """Is `filename` compiled by this source text?

    A unit test inside a crate's `src/` is only compiled when something declares
    it, and this codebase declares them two ways: `mod <stem>;` beside the file,
    and `#[path = "tests/<file>.rs"]` followed by a module whose name differs
    from the file. Accepting only the first form would reject legitimate
    evidence and make the check worse than the hole it closes.

    Integration tests under a crate's `tests/` directory are discovered by cargo
    and need no declaration at all.
    """
    if re.search(rf"^\s*(?:pub\s+)?mod\s+{re.escape(stem)}\s*;", text, re.M):
        return True
    return re.search(rf'#\[path\s*=\s*"[^"]*{re.escape(filename)}"\]', text) is not None


def evidence_can_run(cited, root=None):
    """Is this cited unit test compiled? Integration tests need no declaration."""
    if not cited:
        return True
    root = root or ROOT
    parts = list(PurePath(cited).parts)
    path = root / cited
    if not path.is_file() or "src" not in parts:
        return True  # existence is checked separately; tests/ is auto-discovered
    if parts[-1] in ("lib.rs", "main.rs", "mod.rs"):
        return True
    idx = parts.index("src")
    crate = root / PurePath(*parts[:idx]) if idx else root
    for candidate in sorted(crate.glob("src/**/*.rs")):
        try:
            if declared_in(candidate.read_text(errors="ignore"), path.stem, path.name):
                return True
        except OSError:
            continue
    return False


def validate(record, vector):
    """What is missing from this verdict record, if anything."""
    problems = []
    if record.get("vector") != vector:
        problems.append(f"vector must be {vector!r}")
    verdict = record.get("verdict")
    if verdict not in ("delivered", "not-delivered"):
        problems.append("verdict must be 'delivered' or 'not-delivered'")
    elif verdict == "delivered":
        evidence = str(record.get("evidence") or "").strip()
        if not evidence:
            problems.append("a delivered verdict needs evidence: the test or record that proves it")
        elif not evidence_exists(evidence):
            problems.append(
                f"evidence `{evidence}` names nothing in this checkout (a path that does not "
                f"exist, or an identifier no file contains)"
            )
        elif not evidence_can_run(cited_path(evidence)):
            problems.append(
                f"evidence `{evidence}` is a unit test no module declares, so it never compiles "
                f"and proves nothing: declare it (mod {PurePath(cited_path(evidence)).stem};) "
                f"in the crate and add that file to your claim's scopes"
            )
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
            "evidence": "scripts/check-roadmap-verdict.py", "recorded_by": "TEST", "recorded_unix": 1,
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
            ("delivered naming nothing real", {**good_delivered, "evidence": "surely_it_works"}, 1),
            ("refuted with nothing tracking it", {**good_refuted, "tracked_by": []}, 1),
            ("a refutation needs no evidence at all", {**good_refuted, "evidence": ""}, 0),
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
        # The reachability rule, on the two shapes that matter.
        cases = [
            ("a `mod <stem>;` declaration counts",
             declared_in("mod declared_one;", "declared_one", "declared_one.rs"), True),
            ("a `pub mod` declaration counts",
             declared_in("pub mod helper;", "helper", "helper.rs"), True),
            ("a `#[path]` declaration counts when the module name differs",
             declared_in('#[path = "tests/vc_201_051.rs"]\nmod vc_201_051_tests;',
                         "vc_201_051", "vc_201_051.rs"), True),
            ("an undeclared unit test cannot run",
             declared_in("mod something_else;", "never_declared", "never_declared.rs"), False),
            ("`id (path)` cites the path",
             cited_path("vc_x_mastery_y (crates/a/src/tests/vc_x.rs)"),
             "crates/a/src/tests/vc_x.rs"),
            ("a bare path cites itself", cited_path("crates/a/src/tests/vc_x.rs"),
             "crates/a/src/tests/vc_x.rs"),
            ("an identifier alone cites no path", cited_path("vc_x_mastery_y"), ""),
        ]
        for name, got, want in cases:
            ok = got == want
            if not ok:
                failures += 1
            print(f"  self-test: {name} → {got} (expected {want}) {'ok' if ok else 'WRONG'}")
        if failures:
            print(f"self-test FAILED: {failures} case(s) wrong")
            return 1
        print("PASS: a verdict closes on evidence that can run, or on a refutation")
        return 0


def check_all():
    """Validate every recorded verdict — for CI, where a vacuous one must not land.

    A verdict is the acceptance of a verification task, so it has to hold on main
    and not only in the worktree that wrote it: a delivery whose cited test no
    module declares would otherwise land a false claim about the code.
    """
    records = sorted(VERDICTS.glob("*.json")) if VERDICTS.is_dir() else []
    if not records:
        print("✅ no verdicts recorded yet")
        return 0
    failures = 0
    for path in records:
        try:
            record = json.loads(path.read_text())
        except ValueError as err:
            failures += 1
            print(f"❌ {path.name}: not valid JSON ({err})")
            continue
        vector = record.get("vector") or path.stem
        problems = validate(record, vector)
        if problems:
            failures += 1
            print(f"❌ {path.name}:")
            for problem in problems:
                print(f"   - {problem}")
        else:
            print(f"✅ {path.name}: {record.get('verdict')} — the evidence holds")
    return 1 if failures else 0


def main():
    argv = sys.argv[1:]
    if not argv:
        print("usage: check-roadmap-verdict.py <VC-id> | --self-test | --all", file=sys.stderr)
        return 2
    if argv[0] == "--self-test":
        return self_test()
    if argv[0] == "--all":
        return check_all()
    return check(argv[0])


if __name__ == "__main__":
    sys.exit(main())
