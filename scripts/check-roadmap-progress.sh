#!/usr/bin/env bash
# Roadmap progress must not contradict verified delivery.
#
# `coverage is not completion` (AGENTS.md): a vector is done when its
# mastery_target is verified, not merely when its linked tasks close. The
# inverse also holds — a vector whose linked tasks are closed, each having
# passed the acceptance check that closed it, cannot still claim
# "not an implemented capability". That placeholder was the state of 100 of
# 102 vectors while every one of them had green linked acceptance checks, so
# the roadmap read as unbuilt work that was in fact implemented and tested.
#
# This gate fails while any vector referenced by a closed task still carries
# that placeholder, and also checks the file's own integrity (unique ids,
# known priorities, required strings, resolvable depends_on).
#
#   scripts/check-roadmap-progress.sh
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
roadmap="$root/.agents/roadmap.json"
tasks="$root/.agents/tasks/done"

fail=0
err() { echo "❌ roadmap: $*" >&2; fail=1; }

[ -f "$roadmap" ] || { echo "❌ roadmap: $roadmap not found" >&2; exit 1; }

# 1. Well-formed and internally consistent.
if ! jq empty "$roadmap" 2>/dev/null; then
    err "$roadmap is not valid JSON"
    exit 1
fi
if ! jq -e '
      (.schema == "susi/roadmap/v1")
  and ([.vectors[].id] | length == (unique | length))
  and ([.vectors[] | select(.id | test("^VC-") | not)] | length == 0)
  and ([.vectors[] | select(.priority | IN("P0","P1","P2") | not)] | length == 0)
  and ([.vectors[] | select(
          (.vector | type != "string" or length == 0)
       or (.mastery_target | type != "string" or length == 0)
       or (.progress | type != "string" or length == 0))] | length == 0)
' "$roadmap" >/dev/null; then
    err "schema/field/priority/uniqueness check failed (see .agents/schemas/roadmap.schema.json)"
fi

ids=$(jq -r '.vectors[].id' "$roadmap" | sort -u)
dangling=$(jq -r '.vectors[] | .id as $id | (.depends_on // [])[] | "\($id) -> \(.)"' "$roadmap" \
    | while read -r from _ to; do
        grep -qx "$to" <<<"$ids" || echo "$from -> $to"
      done)
if [ -n "$dangling" ]; then
    err "depends_on targets that are not vectors: $(echo "$dangling" | tr '\n' ' ')"
fi

# 2. No vector with closed linked tasks may still deny being implemented.
refs=$(jq -r '.roadmap? // empty' "$tasks"/*.json 2>/dev/null | sort -u)
stale=$(jq -r --arg refs "$refs" '
    ($refs | split("\n") | map(select(length > 0))) as $r
  | .vectors[]
  | select(.id as $i | $r | index($i))
  | select(.progress | test("not an implemented capability"))
  | .id' "$roadmap")

total=$(jq '.vectors | length' "$roadmap")
remaining=$(grep -c "not an implemented capability" "$roadmap" || true)
if [ -n "$stale" ]; then
    count=$(echo "$stale" | grep -c .)
    err "$count vector(s) with closed linked tasks still say 'not an implemented capability': $(echo "$stale" | tr '\n' ' ')"
    echo "   $((total - remaining))/$total reconciled; $remaining placeholder(s) left" >&2
fi

if [ "$fail" = 0 ]; then
    echo "✅ roadmap progress: all $total vectors reconciled against their linked closed tasks"
fi
exit "$fail"
