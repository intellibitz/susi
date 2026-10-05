#!/usr/bin/env python3
"""Measure the self-build loop without turning missing evidence into success.

The queue records when a task was proposed and accepted, git records the first
implementation commit, and the merge job records the tested head and merge
commit in ``refs/merged/<task>``.  Release and promotion are intentionally
separate lifecycle events: they are read from a small JSONL ledger when the
workflow emits them.  Keeping those sources separate makes a missing release
visible instead of silently treating a merged task as released.

The optional lifecycle ledger is one JSON object per line::

    {"task": "T-AGENT-1", "phase": "released", "at_unix": 1710000300,
     "cost": 0.12, "cost_unit": "runner_minutes"}
    {"task": "T-AGENT-1", "phase": "defect", "at_unix": 1710000200,
     "outcome": "failed", "detail": "branch gate"}

Usage::

    scripts/check-rsi-scorecard.py --task T-AGENT-1 --events .agents/rsi-loop.jsonl
    scripts/check-rsi-scorecard.py --json
    scripts/check-rsi-scorecard.py --self-test

The default report is deliberately useful while work is in flight: it shows
the first missing phase and the evidence observed so far.  ``--require-complete``
turns that report into a gate for callers that need the entire loop.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, Iterable


ROOT = Path(__file__).resolve().parent.parent
PHASES = ("proposed", "implemented", "gated", "merged", "released", "promoted")
EVENT_FILE = Path(".agents/rsi-loop.jsonl")


def git(*args: str, cwd: Path) -> str:
    """Return git output, or an empty string when this is not a git checkout."""
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    return result.stdout if result.returncode == 0 else ""


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def task_records(root: Path) -> list[dict[str, Any]]:
    """Load queue records from the working tree, including closed tasks."""
    records: list[dict[str, Any]] = []
    for directory in (root / ".agents/tasks", root / ".agents/tasks/done"):
        if not directory.is_dir():
            continue
        for path in sorted(directory.glob("*.json")):
            record = read_json(path)
            if isinstance(record, dict) and record.get("id"):
                records.append(record)
    # A task moves from the open directory to done; prefer the latter if a
    # checkout contains both during a worktree operation.
    unique: dict[str, dict[str, Any]] = {}
    for record in records:
        unique[str(record["id"])] = record
    return [unique[key] for key in sorted(unique)]


def load_events(path: Path | None) -> tuple[list[dict[str, Any]], list[str]]:
    """Read lifecycle events and retain malformed-line diagnostics."""
    if path is None or not path.is_file():
        return [], []
    problems: list[str] = []
    events: list[dict[str, Any]] = []
    try:
        lines = path.read_text().splitlines()
    except OSError as error:
        return [], [f"cannot read {path}: {error}"]
    for line_number, line in enumerate(lines, 1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except ValueError as error:
            problems.append(f"{path}:{line_number}: invalid JSON: {error}")
            continue
        if not isinstance(event, dict):
            problems.append(f"{path}:{line_number}: event must be a JSON object")
            continue
        events.append(event)
    return events, problems


def event_task(event: dict[str, Any]) -> str:
    return str(event.get("task") or event.get("task_id") or "").strip()


def event_time(event: dict[str, Any]) -> int | None:
    value = event.get("at_unix", event.get("timestamp"))
    if isinstance(value, bool):
        return None
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return None
    return parsed if parsed >= 0 else None


def task_commits(root: Path, task_ids: Iterable[str]) -> dict[str, int]:
    """Find the earliest commit carrying each task trailer."""
    wanted = set(task_ids)
    found: dict[str, int] = {}
    if not wanted:
        return found
    output = git(
        "log",
        "--all",
        "--format=%H%x1f%ct%x1f%B%x1e",
        "--reverse",
        cwd=root,
    )
    for record in output.split("\x1e"):
        fields = record.split("\x1f", 2)
        if len(fields) != 3:
            continue
        try:
            timestamp = int(fields[1])
        except ValueError:
            continue
        for line in fields[2].splitlines():
            if not line.startswith("Task:"):
                continue
            task = line.partition(":")[2].strip()
            if task in wanted and task not in found:
                found[task] = timestamp
    return found


def merge_attestation(root: Path, task: str) -> dict[str, Any] | None:
    """Read the merge job's attestation when it is available locally."""
    output = git("cat-file", "-p", f"refs/merged/{task}", cwd=root).strip()
    if not output:
        return None
    try:
        record = json.loads(output)
    except ValueError:
        return None
    return record if isinstance(record, dict) else None


def first_phase_events(events: Iterable[dict[str, Any]]) -> tuple[dict[str, int], int, str | None]:
    phases: dict[str, int] = {}
    cost = 0.0
    cost_unit: str | None = None
    for event in events:
        phase = str(event.get("phase") or "").strip().lower()
        timestamp = event_time(event)
        if phase in PHASES and timestamp is not None and phase not in phases:
            phases[phase] = timestamp
        raw_cost = event.get("cost")
        if raw_cost is not None:
            try:
                parsed_cost = float(raw_cost)
            except (TypeError, ValueError):
                continue
            if parsed_cost < 0:
                continue
            cost += parsed_cost
            candidate_unit = str(event.get("cost_unit") or "units").strip() or "units"
            if cost_unit is None:
                cost_unit = candidate_unit
            elif cost_unit != candidate_unit:
                # Mixed units cannot be added honestly.  Keep the value but
                # make the unit explicit so the report cannot be misread.
                cost_unit = "mixed"
    return phases, cost, cost_unit


def score_task(
    task: dict[str, Any],
    events: Iterable[dict[str, Any]],
    implementation_unix: int | None,
    attestation: dict[str, Any] | None,
) -> dict[str, Any]:
    task_id = str(task.get("id") or "")
    phases, cost, cost_unit = first_phase_events(events)

    created = task.get("created_unix")
    try:
        proposed = int(created)
    except (TypeError, ValueError):
        proposed = None
    if proposed is not None and proposed >= 0:
        phases.setdefault("proposed", proposed)
    if implementation_unix is not None:
        phases.setdefault("implemented", implementation_unix)

    closed = task.get("closed")
    if isinstance(closed, dict):
        try:
            closed_unix = int(closed.get("at_unix"))
        except (TypeError, ValueError):
            closed_unix = None
        if closed_unix is not None and closed_unix >= 0:
            phases.setdefault("gated", closed_unix)

    if attestation:
        try:
            merged_unix = int(attestation.get("merged_unix"))
        except (TypeError, ValueError):
            merged_unix = None
        if merged_unix is not None and merged_unix >= 0:
            phases.setdefault("merged", merged_unix)

    defects = 0
    for event in events:
        phase = str(event.get("phase") or "").strip().lower()
        outcome = str(event.get("outcome") or event.get("status") or "").strip().lower()
        if phase == "defect" or outcome in {"failed", "failure", "defect"}:
            defects += 1
    declared_defects = task.get("defects")
    if isinstance(declared_defects, list):
        defects += len(declared_defects)

    missing = [phase for phase in PHASES if phase not in phases]
    problems: list[str] = []
    ordered = [(phase, phases[phase]) for phase in PHASES if phase in phases]
    for (left, left_time), (right, right_time) in zip(ordered, ordered[1:]):
        if right_time < left_time:
            problems.append(f"{right} ({right_time}) precedes {left} ({left_time})")

    observed = [timestamp for timestamp in phases.values() if timestamp is not None]
    elapsed = max(observed) - min(observed) if observed else None
    complete = not missing and not problems
    return {
        "task": task_id,
        "title": str(task.get("title") or ""),
        "status": "complete" if complete else ("invalid" if problems else "in_progress"),
        "phases": {phase: phases.get(phase) for phase in PHASES},
        "missing": missing,
        "stop_at": missing[0] if missing else None,
        "elapsed_seconds": elapsed,
        "cost": None if cost_unit is None else {"value": cost, "unit": cost_unit},
        "defects": defects,
        "problems": problems,
    }


def score(root: Path, selected: set[str] | None = None, event_path: Path | None = None) -> dict[str, Any]:
    tasks = task_records(root)
    if selected:
        tasks = [task for task in tasks if task.get("id") in selected]
    events, event_problems = load_events(event_path)
    by_task: dict[str, list[dict[str, Any]]] = {}
    for event in events:
        task_id = event_task(event)
        if task_id:
            by_task.setdefault(task_id, []).append(event)
    task_ids = [str(task["id"]) for task in tasks]
    commits = task_commits(root, task_ids)
    rows = []
    for task in tasks:
        task_id = str(task["id"])
        rows.append(
            score_task(
                task,
                by_task.get(task_id, []),
                commits.get(task_id),
                merge_attestation(root, task_id),
            )
        )
    return {"phases": list(PHASES), "tasks": rows, "problems": event_problems}


def format_seconds(value: int | None) -> str:
    if value is None:
        return "—"
    minutes, seconds = divmod(value, 60)
    hours, minutes = divmod(minutes, 60)
    if hours:
        return f"{hours}h {minutes:02d}m"
    if minutes:
        return f"{minutes}m {seconds:02d}s"
    return f"{seconds}s"


def print_report(report: dict[str, Any]) -> None:
    rows = report["tasks"]
    print("RSI self-build scorecard")
    print("task                         status       elapsed   cost              defects  stop")
    print("---------------------------  -----------  --------  ----------------  -------  --------")
    for row in rows:
        cost = row["cost"]
        cost_text = "—" if cost is None else f'{cost["value"]:g} {cost["unit"]}'
        stop = row["stop_at"] or "—"
        print(
            f'{row["task"][:27]:27}  {row["status"]:11}  '
            f'{format_seconds(row["elapsed_seconds"]):8}  {cost_text:16}  '
            f'{row["defects"]:7}  {stop}'
        )
    if report["problems"]:
        print("\nLedger problems:")
        for problem in report["problems"]:
            print(f"  - {problem}")
    if not rows:
        print("(no task records found)")


def self_test() -> int:
    """Exercise complete, partial, costed, defective and invalid loops."""
    complete_task = {
        "id": "T-TEST-1",
        "title": "complete",
        "created_unix": 100,
        "closed": {"at_unix": 130},
    }
    complete_events = [
        {"task": "T-TEST-1", "phase": "released", "at_unix": 150, "cost": 2, "cost_unit": "runner_minutes"},
        {"task": "T-TEST-1", "phase": "promoted", "at_unix": 160, "cost": 0.5, "cost_unit": "runner_minutes"},
        {"task": "T-TEST-1", "phase": "defect", "at_unix": 125, "outcome": "failed"},
    ]
    scored = score_task(
        complete_task,
        complete_events,
        110,
        {"merged_unix": 140},
    )
    checks = [
        (scored["status"] == "complete", "complete loop is complete"),
        (scored["elapsed_seconds"] == 60, "elapsed time spans proposal to promotion"),
        (scored["cost"] == {"value": 2.5, "unit": "runner_minutes"}, "cost is summed with its unit"),
        (scored["defects"] == 1, "failed lifecycle event is a defect"),
        (not scored["missing"], "complete loop has no missing phase"),
    ]

    partial = score_task(
        {"id": "T-TEST-2", "title": "partial", "created_unix": 200},
        [],
        None,
        None,
    )
    checks.extend(
        [
            (partial["status"] == "in_progress", "partial loop is in progress"),
            (partial["stop_at"] == "implemented", "partial loop stops at implementation"),
            (partial["cost"] is None, "unmeasured cost is not reported as zero"),
        ]
    )

    invalid = score_task(
        {"id": "T-TEST-3", "title": "invalid", "created_unix": 300},
        [{"phase": "promoted", "at_unix": 290}],
        None,
        None,
    )
    checks.append((invalid["status"] == "invalid", "out-of-order phases are invalid"))

    failures = [name for ok, name in checks if not ok]
    for ok, name in checks:
        print(f"  self-test: {name} — {'ok' if ok else 'FAIL'}")
    if failures:
        print(f"self-test FAILED: {len(failures)} check(s)")
        return 1
    print("PASS: the scorecard measures progress and stops on missing evidence")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--task", action="append", help="limit the report to this task id (repeatable)")
    parser.add_argument("--events", type=Path, help="JSONL lifecycle ledger")
    parser.add_argument("--repo", type=Path, default=ROOT, help="repository root")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    parser.add_argument("--require-complete", action="store_true", help="fail unless every selected loop is complete")
    parser.add_argument("--self-test", action="store_true", help="run the deterministic checker self-test")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    if args.self_test:
        return self_test()
    root = args.repo.resolve()
    event_path = args.events
    if event_path is None:
        candidate = root / EVENT_FILE
        event_path = candidate if candidate.is_file() else None
    report = score(root, set(args.task) if args.task else None, event_path)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print_report(report)
    if report["problems"]:
        return 1
    if args.require_complete and any(row["status"] != "complete" for row in report["tasks"]):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
