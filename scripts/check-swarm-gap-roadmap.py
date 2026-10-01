#!/usr/bin/env python3
"""Validate the reviewed swarm gap queue, including closed records after repair."""
import json
from pathlib import Path


def main():
    root = Path(__file__).resolve().parent.parent
    queue = root / ".agents/tasks"
    vectors = json.loads((root / ".agents/roadmap.json").read_text())["vectors"]
    by_vector = {v["id"]: v for v in vectors}
    assert len(by_vector) == len(vectors), "duplicate roadmap vector IDs"
    records = {}
    for path in [*queue.glob("*.json"), *queue.joinpath("done").glob("*.json")]:
        record = json.loads(path.read_text())
        task_id = record["id"]
        assert path.stem == task_id, f"filename disagrees with ID: {path}"
        assert task_id not in records, f"duplicate task: {task_id}"
        records[task_id] = record
    expected = [
        (2, "VC-201-029", "swarm_gap_workspace_isolation", []),
        (3, "VC-201-022", "swarm_gap_durable_fencing", []),
        (4, "VC-201-026", "swarm_gap_real_cancellation", []),
        (5, "VC-200-001", "swarm_gap_durable_quorum", []),
        (6, "VC-201-024", "swarm_gap_live_admission", []),
        (7, "VC-201-023", "swarm_gap_safe_replay", [3]),
        (8, "VC-201-028", "swarm_gap_verified_closure", [2]),
        (9, "VC-201-058", "swarm_gap_production_lifecycle", list(range(2, 9))),
    ]
    for number, vector, test_filter, dependencies in expected:
        task_id = f"T-CODEXSWARMGAPS-{number}"
        task = records[task_id]
        assert task["roadmap"] == vector, f"incorrect roadmap link: {task_id}"
        assert task_id in by_vector[vector]["progress"], f"missing gap status: {vector}"
        assert task["deps"] == [f"T-CODEXSWARMGAPS-{n}" for n in dependencies]
        assert task["accept"]["cmd"] == [
            "cargo", "test", "-p", "susi-gawd-swarm", test_filter, "--locked"
        ], f"incorrect acceptance: {task_id}"
        assert "nonzero" in task["goal"] and "production" in task["goal"]
    visiting, visited = set(), set()

    def visit(task_id):
        assert task_id in records, f"missing dependency: {task_id}"
        assert task_id not in visiting, f"dependency cycle: {task_id}"
        if task_id in visited:
            return
        visiting.add(task_id)
        for dependency in records[task_id]["deps"]:
            visit(dependency)
        visiting.remove(task_id)
        visited.add(task_id)

    for number, _, _, _ in expected:
        visit(f"T-CODEXSWARMGAPS-{number}")
    print("PASS: eight swarm gap tasks, roadmap links, acceptance commands and dependencies")


if __name__ == "__main__":
    main()
