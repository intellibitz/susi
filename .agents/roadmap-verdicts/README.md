# Roadmap mastery verdicts

One file per vector, `<VC-id>.json`, recording whether that vector's
`mastery_target` is actually delivered — or refuted, with the remaining work
tracked. It exists because a verification task cannot close on a passed test
when the honest answer is "this was never delivered": the agent would hold a
dead claim with its work done, and the queue would jam. The verdict is the
result, so the verdict is what closes the task.

One file per vector also keeps thirty agents off a single `roadmap.json`, which
has no merge driver — hand-resolved conflicts in that file are what stranded
the swarm-gap branch for a day.

```json
{
  "vector": "VC-201-071",
  "verdict": "delivered",
  "evidence": "vc_201_071_mastery_policy_decision (crates/.../policy.rs)",
  "missing": "",
  "tracked_by": [],
  "recorded_by": "CURSOR20261002053120",
  "recorded_unix": 1790958000
}
```

- `delivered` requires `evidence`: the test, record or command that proves the
  capability on its production path.
- `not-delivered` requires `missing` (what the vector still lacks) and
  `tracked_by` (the task id(s) that will close it), each of which must exist in
  the queue or `done/`.
- `recorded_by` and `recorded_unix` are always required.

Check one, or pin the rules themselves:

```sh
scripts/check-roadmap-verdict.py VC-201-071
scripts/check-roadmap-verdict.py --self-test
```

`scripts/check-roadmap-queue.py` counts a recorded verdict as resolved, so a
refuted vector is not reported as unqueued work forever.
