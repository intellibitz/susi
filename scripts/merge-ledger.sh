#!/usr/bin/env bash
# Git custom merge driver for .agents/evidence.json (`merge=ledger`).
#
# Called by git as: scripts/merge-ledger.sh %O %A %B
#   %O = ancestor, %A = ours (write result here), %B = theirs
#
# Both sides only append EV-* entries to `.entries` and bump `.total_entries`.
# Take our file as the base, append any entry from theirs whose `id` is not
# already present, recompute `total_entries`, and write back to %A.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

base=$1
ours=$2
theirs=$3
: "$base"

jq -e --slurp --indent 2 '
  .[0] as $o | .[1] as $t |
  if ($o | type) != "object" or ($t | type) != "object" then error("not object")
  elif ($o.entries | type) != "array" or ($t.entries | type) != "array" then error("entries not array")
  else
    ($o.entries | map(select(type == "object" and (.id | type) == "string") | {key: .id, value: true}) | from_entries) as $seen |
    ($o.entries + [
      $t.entries[] |
      select(type == "object" and (.id | type) == "string" and ($seen[.id] | not))
    ]) as $merged |
    $o | .entries = $merged | .total_entries = ($merged | length)
  end
' "$ours" "$theirs" > "$ours.tmp" || {
  rm -f "$ours.tmp"
  exit 1
}
mv "$ours.tmp" "$ours"
