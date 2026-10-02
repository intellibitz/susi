#!/usr/bin/env python3
"""Git merge driver for `.agents/roadmap.json`.

Usage (registered by scripts/setup-dev.sh and .gitattributes):
    merge-roadmap.py BASE OURS THEIRS

Every verification task updates one vector's `progress` narrative when it
records a verdict, so two agents merging at once usually touch different
vectors — and git's line-based merge only sees that the *file* changed. Worse,
an agent (or a tool) that rewrites the whole JSON — reordering keys, changing
`ensure_ascii` — turns a one-line edit into a whole-file conflict. That is what
stranded the swarm-gap branch for a day.

So this merges by vector id instead of by line:

* a vector changed on one side only is taken from that side;
* vectors changed independently on both sides are both kept;
* the vector changed on *both* sides differently is a genuine disagreement: the
  result keeps OURS for it and the driver exits 1, so git marks the file
  conflicted and the agent resolves that one vector deliberately;
* a vector added on either side is kept, and top-level fields follow the same
  three-way rule.

Serialization matches `.agents/roadmap.json` byte-for-byte in shape (indent 2,
sorted keys, UTF-8, trailing newline), so a merge produces no formatting churn.
Unparseable input exits 1, letting git report an ordinary conflict rather than
writing a damaged file.
"""
import json
import sys
import tempfile
from pathlib import Path

VECTORS = "vectors"


def load(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def dump(path, doc):
    text = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)


def three_way(base, ours, theirs, label, conflicts):
    """Ours unless only theirs changed; both changing differently is a conflict."""
    if ours == theirs:
        return ours
    if ours == base:
        return theirs
    if theirs == base:
        return ours
    conflicts.append(label)
    return ours


def merge(base, ours, theirs):
    conflicts = []
    result = {}
    for key in sorted(set(base) | set(ours) | set(theirs)):
        if key == VECTORS:
            continue
        result[key] = three_way(
            base.get(key), ours.get(key), theirs.get(key), f"field {key}", conflicts
        )

    def as_list(doc):
        return [v for v in doc.get(VECTORS, []) if isinstance(v, dict) and "id" in v]

    ours_vectors, theirs_vectors = as_list(ours), as_list(theirs)
    base_by_id = {v["id"]: v for v in as_list(base)}
    ours_by_id = {v["id"]: v for v in ours_vectors}
    theirs_by_id = {v["id"]: v for v in theirs_vectors}

    merged = []
    for vector in ours_vectors:  # ours sets the order; theirs-only ids append
        vid = vector["id"]
        merged.append(
            three_way(
                base_by_id.get(vid),
                ours_by_id.get(vid),
                theirs_by_id.get(vid),
                f"vector {vid}",
                conflicts,
            )
        )
    for vector in theirs_vectors:
        if vector["id"] not in ours_by_id:
            merged.append(vector)
    # A vector deleted on one side and kept on the other stays (never delete).
    for vid, vector in base_by_id.items():
        if vid not in ours_by_id:
            merged.append(theirs_by_id.get(vid, vector))
    result[VECTORS] = merged
    return result, conflicts


def self_test():
    """Pin the five behaviours this driver exists for."""
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)

        def write(name, doc):
            path = root / name
            dump(path, doc)
            return path

        base = {"version": 1, "vectors": [{"id": "VC-1-1", "progress": "PARTIAL"}]}
        cases = []

        ours = {"version": 1, "vectors": [{"id": "VC-1-1", "progress": "DELIVERED"}]}
        cases.append(("ours only", base, ours, base, 0, "DELIVERED"))

        theirs = {"version": 1, "vectors": [{"id": "VC-1-1", "progress": "NOT DELIVERED"}]}
        cases.append(("theirs only", base, base, theirs, 0, "NOT DELIVERED"))

        added = {
            "version": 1,
            "vectors": [
                {"id": "VC-1-1", "progress": "PARTIAL"},
                {"id": "VC-1-2", "progress": "PLANNED"},
            ],
        }
        cases.append(("a vector added by theirs", base, base, added, 0, "VC-1-2"))

        field = {"version": 2, "vectors": base["vectors"]}
        cases.append(("a top-level field changed by theirs", base, base, field, 0, None))

        both = {"version": 1, "vectors": [{"id": "VC-1-1", "progress": "DELIVERED"}]}
        other = {"version": 1, "vectors": [{"id": "VC-1-1", "progress": "REFUTED"}]}
        cases.append(("the same vector changed on both sides", base, both, other, 1, "DELIVERED"))

        failures = 0
        for name, b, o, t, expected, expect_text in cases:
            base_p = write("base.json", b)
            ours_p = write("ours.json", o)
            theirs_p = write("theirs.json", t)
            merged, conflicts = merge(load(base_p), load(ours_p), load(theirs_p))
            code = 1 if conflicts else 0
            text = json.dumps(merged, ensure_ascii=False)
            ok = code == expected and (expect_text is None or expect_text in text)
            if not ok:
                failures += 1
            if expected == 1 and not conflicts:
                failures += 1
            print(f"  self-test: {name} → exit {code} (expected {expected}) "
                  f"{'ok' if ok else 'WRONG'}")
        if failures:
            print(f"self-test FAILED: {failures} case(s) wrong")
            return 1
        print("PASS: the roadmap merges per vector, and fails only on a real disagreement")
        return 0


def main():
    argv = sys.argv[1:]
    if argv[:1] == ["--self-test"]:
        return self_test()
    if len(argv) < 3:
        print("usage: merge-roadmap.py BASE OURS THEIRS | --self-test", file=sys.stderr)
        return 2
    base_p, ours_p, theirs_p = argv[0], argv[1], argv[2]
    try:
        base, ours, theirs = load(base_p), load(ours_p), load(theirs_p)
    except (OSError, ValueError):
        return 1
    merged, conflicts = merge(base, ours, theirs)
    dump(ours_p, merged)
    if conflicts:
        print(
            "roadmap merge: both sides changed " + ", ".join(conflicts)
            + " — kept ours there; resolve those deliberately (the rest merged)",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
