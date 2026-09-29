#!/usr/bin/env python3
"""Git merge driver for the append-only `.agents/evidence.json` ledger.

Usage (registered by scripts/setup-dev.sh): merge-ledger.py BASE OURS THEIRS

Two agents that each append an entry produce a textual conflict at the tail
of `entries`. This driver keeps OURS byte-for-byte and splices in every
entry THEIRS added (id absent from BASE and OURS) before the array closes.
Entries are never deleted or renumbered. Non-entry fields (scores,
last_verification) keep OURS. Exit 1 on unparseable input so git reports a
normal conflict instead of silently writing a bad file.
"""
import json
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    return text, json.loads(text)


def main():
    base_p, ours_p, theirs_p = sys.argv[1:4]
    try:
        _, base = load(base_p)
        ours_text, ours = load(ours_p)
        _, theirs = load(theirs_p)
    except (OSError, ValueError):
        return 1
    seen = {e.get("id") for e in base["entries"]} | {e.get("id") for e in ours["entries"]}
    added = [e for e in theirs["entries"] if e.get("id") not in seen]
    if not added:
        return 0
    marker = "\n ],\n"
    idx = ours_text.find(marker)
    if idx < 0:
        return 1
    chunks = []
    for e in added:
        body = json.dumps(e, indent=1)
        chunks.append("\n".join("  " + line for line in body.splitlines()))
    merged = ours_text[:idx] + ",\n" + ",\n".join(chunks) + ours_text[idx:]
    try:
        json.loads(merged)
    except ValueError:
        return 1
    with open(ours_p, "w", encoding="utf-8") as f:
        f.write(merged)
    return 0


sys.exit(main())
