#!/usr/bin/env python3
"""Validate the VC-203 GMCP completion program and its task dependency proof."""

from __future__ import annotations

import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
ROADMAP = ROOT / ".agents" / "roadmap.json"
TASK_ROOT = ROOT / ".agents" / "tasks"
DOC = ROOT / "docs" / "gmcp-roadmap.md"
README = ROOT / "README.md"

VECTOR_IDS = tuple(f"VC-203-{number:03d}" for number in range(1, 49))
VECTOR_SET = set(VECTOR_IDS)
SUFFIX_TEST = {
    "01": "contract",
    "02": "production",
    "03": "integration",
    "04": "adversarial",
    "05": "interop",
    "06": "operations",
}
REQUIREMENT_RE = re.compile(r"\bRequirement (GMCP-203-(\d{3})-(01|02|03|04|05|06|M))\b")


class Validation:
    def __init__(self) -> None:
        self.errors: list[str] = []

    def require(self, condition: bool, message: str) -> None:
        if not condition:
            self.errors.append(message)

    def finish(self, vector_count: int, task_count: int) -> None:
        if self.errors:
            for error in self.errors:
                print(f"GMCP roadmap error: {error}", file=sys.stderr)
            print(f"GMCP roadmap invalid: {len(self.errors)} error(s)", file=sys.stderr)
            raise SystemExit(1)
        print(
            "GMCP roadmap OK: "
            f"{vector_count} vectors, {task_count} requirement tasks, "
            "acyclic dependencies, complete mastery fan-in"
        )


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read valid JSON from {path.relative_to(ROOT)}: {error}") from error


def load_tasks(check: Validation) -> tuple[dict[str, dict[str, Any]], set[str]]:
    tasks: dict[str, dict[str, Any]] = {}
    done: set[str] = set()
    locations = ((TASK_ROOT, False), (TASK_ROOT / "done", True))
    for directory, is_done in locations:
        for path in sorted(directory.glob("*.json")):
            value = load_json(path)
            task_id = value.get("id")
            check.require(isinstance(task_id, str), f"{path.relative_to(ROOT)} has no string id")
            if not isinstance(task_id, str):
                continue
            check.require(task_id not in tasks, f"duplicate task id {task_id}")
            tasks[task_id] = value
            if is_done:
                done.add(task_id)
    return tasks, done


def assert_acyclic(
    check: Validation,
    names: set[str],
    edges: dict[str, list[str]],
    label: str,
) -> None:
    state: dict[str, int] = {}
    trail: list[str] = []

    def visit(node: str) -> None:
        mark = state.get(node, 0)
        if mark == 2:
            return
        if mark == 1:
            try:
                start = trail.index(node)
            except ValueError:
                start = 0
            check.errors.append(f"{label} dependency cycle: {' -> '.join(trail[start:] + [node])}")
            return
        state[node] = 1
        trail.append(node)
        for dependency in edges.get(node, []):
            if dependency in names:
                visit(dependency)
        trail.pop()
        state[node] = 2

    for name in sorted(names):
        visit(name)


def expected_requirement_ids(vector_id: str) -> set[str]:
    number = vector_id.rsplit("-", 1)[1]
    return {f"GMCP-203-{number}-{suffix}" for suffix in (*SUFFIX_TEST, "M")}


def expected_test_name(requirement_id: str) -> str:
    _, _, number, suffix = requirement_id.split("-")
    if suffix == "M":
        if number == "048":
            return "gmcp_complete_mastery"
        return f"gmcp_vc_203_{number}_mastery"
    return f"gmcp_req_203_{number}_{suffix}_{SUFFIX_TEST[suffix]}"


def main() -> None:
    check = Validation()
    roadmap = load_json(ROADMAP)
    vectors = roadmap.get("vectors", [])
    check.require(isinstance(vectors, list), ".agents/roadmap.json vectors must be a list")
    if not isinstance(vectors, list):
        check.finish(0, 0)
        return

    ids = [vector.get("id") for vector in vectors if isinstance(vector, dict)]
    counts = Counter(ids)
    for vector_id, count in sorted(counts.items(), key=lambda item: str(item[0])):
        check.require(count == 1, f"roadmap id {vector_id!r} appears {count} times")

    by_id = {
        vector["id"]: vector
        for vector in vectors
        if isinstance(vector, dict) and isinstance(vector.get("id"), str)
    }
    missing_vectors = sorted(VECTOR_SET - set(by_id))
    check.require(not missing_vectors, f"missing VC-203 vectors: {', '.join(missing_vectors)}")

    vector_edges: dict[str, list[str]] = {}
    for vector_id in VECTOR_IDS:
        vector = by_id.get(vector_id)
        if vector is None:
            continue
        dependencies = vector.get("depends_on")
        check.require(isinstance(dependencies, list), f"{vector_id} depends_on must be a list")
        dependencies = dependencies if isinstance(dependencies, list) else []
        vector_edges[vector_id] = [item for item in dependencies if isinstance(item, str)]
        check.require(vector_id not in dependencies, f"{vector_id} depends on itself")
        for dependency in dependencies:
            check.require(dependency in by_id, f"{vector_id} has unknown dependency {dependency}")
        check.require(vector.get("priority") in {"P0", "P1", "P2"}, f"{vector_id} has invalid priority")
        check.require(bool(vector.get("type")), f"{vector_id} has no type")
        check.require(bool(vector.get("vector")), f"{vector_id} has no capability name")
        check.require(
            len(str(vector.get("mastery_target", ""))) >= 160,
            f"{vector_id} mastery target is not sufficiently explicit",
        )
        progress = str(vector.get("progress", ""))
        check.require(
            progress.startswith("NOT DELIVERED") or progress.startswith("DELIVERED"),
            f"{vector_id} progress must explicitly say NOT DELIVERED or DELIVERED",
        )

    assert_acyclic(check, VECTOR_SET, vector_edges, "roadmap")
    check.require(
        set(vector_edges.get("VC-203-048", [])) == set(VECTOR_IDS[:-1]),
        "VC-203-048 must depend directly on every preceding VC-203 vector",
    )

    tasks, done = load_tasks(check)
    generated: dict[str, dict[str, Any]] = {}
    requirement_to_task: dict[str, str] = {}
    vector_requirements: dict[str, dict[str, str]] = defaultdict(dict)

    for task_id, task in tasks.items():
        goal = str(task.get("goal", ""))
        matches = REQUIREMENT_RE.findall(goal)
        if len(matches) > 1:
            check.errors.append(f"{task_id} declares multiple GMCP requirements")
            continue
        if not matches:
            if str(task.get("title", "")).startswith("[GMCP 203."):
                check.errors.append(f"{task_id} has a GMCP title but no requirement identifier")
            continue
        requirement_id, number, suffix = matches[0]
        vector_id = f"VC-203-{number}"
        check.require(vector_id in VECTOR_SET, f"{task_id} uses out-of-range {requirement_id}")
        check.require(
            task.get("roadmap") == vector_id,
            f"{task_id} maps {requirement_id} to {task.get('roadmap')!r}, expected {vector_id}",
        )
        check.require(requirement_id not in requirement_to_task, f"duplicate requirement {requirement_id}")
        requirement_to_task[requirement_id] = task_id
        vector_requirements[vector_id][suffix] = task_id
        generated[task_id] = task

        command = task.get("accept", {}).get("cmd", [])
        expected_name = expected_test_name(requirement_id)
        expected_command = [
            "cargo",
            "test",
            "--locked",
            "--test",
            "gmcp_requirements",
            expected_name,
        ]
        check.require(
            command == expected_command,
            f"{task_id} acceptance must be {' '.join(expected_command)!r}, got {command!r}",
        )
        check.require(task.get("size") in {"s", "m", "l"}, f"{task_id} has invalid size")
        dependencies = task.get("deps", [])
        check.require(isinstance(dependencies, list), f"{task_id} deps must be a list")
        if isinstance(dependencies, list):
            for dependency in dependencies:
                check.require(dependency in tasks, f"{task_id} has dangling dependency {dependency}")

    expected_all = set()
    for vector_id in VECTOR_IDS:
        expected = expected_requirement_ids(vector_id)
        expected_all.update(expected)
        actual = {
            f"GMCP-203-{vector_id.rsplit('-', 1)[1]}-{suffix}"
            for suffix in vector_requirements.get(vector_id, {})
        }
        missing = sorted(expected - actual)
        check.require(not missing, f"{vector_id} missing requirements: {', '.join(missing)}")

    unexpected = sorted(set(requirement_to_task) - expected_all)
    check.require(not unexpected, f"unexpected GMCP requirements: {', '.join(unexpected)}")
    check.require(
        len(requirement_to_task) == 48 * 7,
        f"expected 336 unique requirement tasks, found {len(requirement_to_task)}",
    )

    task_edges: dict[str, list[str]] = {}
    for task_id, task in generated.items():
        task_edges[task_id] = [
            dependency
            for dependency in task.get("deps", [])
            if isinstance(dependency, str) and dependency in generated
        ]
    assert_acyclic(check, set(generated), task_edges, "task")

    mastery_ids = {
        vector_id: vector_requirements[vector_id]["M"]
        for vector_id in VECTOR_IDS
        if "M" in vector_requirements.get(vector_id, {})
    }
    for vector_id in VECTOR_IDS:
        mapped = vector_requirements.get(vector_id, {})
        if "M" not in mapped:
            continue
        mastery_id = mapped["M"]
        work_ids = {mapped[suffix] for suffix in SUFFIX_TEST if suffix in mapped}
        check.require(
            work_ids.issubset(set(tasks[mastery_id].get("deps", []))),
            f"{mastery_id} must depend on all six work-package tasks",
        )

        contract_id = mapped.get("01")
        if contract_id:
            prerequisite_masteries = {
                mastery_ids[dependency]
                for dependency in vector_edges.get(vector_id, [])
                if dependency in mastery_ids
            }
            check.require(
                prerequisite_masteries.issubset(set(tasks[contract_id].get("deps", []))),
                f"{contract_id} lacks prerequisite vector mastery dependencies",
            )

        expected_internal = {
            "02": {"01"},
            "03": {"01", "02"},
            "04": {"01", "02", "03"},
            "05": {"02", "03", "04"},
            "06": {"01", "02", "03", "04", "05"},
        }
        for suffix, required_suffixes in expected_internal.items():
            task_id = mapped.get(suffix)
            if not task_id:
                continue
            required_ids = {mapped[item] for item in required_suffixes if item in mapped}
            check.require(
                required_ids.issubset(set(tasks[task_id].get("deps", []))),
                f"{task_id} lacks required intra-vector dependencies",
            )

        progress = str(by_id.get(vector_id, {}).get("progress", ""))
        all_done = all(task_id in done for task_id in mapped.values())
        if progress.startswith("DELIVERED"):
            check.require(all_done, f"{vector_id} claims delivery while requirement tasks remain open")
            verdict = ROOT / ".agents" / "roadmap-verdicts" / f"{vector_id}.json"
            check.require(verdict.is_file(), f"{vector_id} claims delivery without a verdict")
        if all_done:
            check.require(
                progress.startswith("DELIVERED"),
                f"{vector_id} has all requirement tasks done but progress is not DELIVERED",
            )

    final_mastery = mastery_ids.get("VC-203-048")
    if final_mastery:
        required_masteries = {mastery_ids[item] for item in VECTOR_IDS[:-1] if item in mastery_ids}
        check.require(
            required_masteries.issubset(set(tasks[final_mastery].get("deps", []))),
            f"{final_mastery} must directly depend on all 47 preceding mastery tasks",
        )

    doc = DOC.read_text(encoding="utf-8") if DOC.is_file() else ""
    readme = README.read_text(encoding="utf-8") if README.is_file() else ""
    check.require(bool(doc), "docs/gmcp-roadmap.md is missing")
    for vector_id in VECTOR_IDS:
        check.require(vector_id in doc, f"docs/gmcp-roadmap.md does not mention {vector_id}")
    for token in (
        "MCP 2026-07-28",
        "336 open implementation",
        "gmcp_complete_mastery",
        "Official conformance",
        "Standards and metric drift loop",
        "zero unresolved critical/high",
    ):
        check.require(token in doc, f"docs/gmcp-roadmap.md lacks required completion token {token!r}")
    check.require(
        "docs/gmcp-roadmap.md" in readme,
        "README.md does not link the GMCP completion roadmap",
    )

    check.finish(len(VECTOR_SET), len(requirement_to_task))


if __name__ == "__main__":
    main()
