#!/usr/bin/env bash
# Git custom merge driver for .agents/roadmap.json (`merge=roadmap`).
#
# Called by git as: scripts/merge-roadmap.sh %O %A %B
#   %O = ancestor, %A = ours (write result here), %B = theirs
#
# Each verification task updates its own vector's `.progress` in roadmap.json,
# so two agents closing two vectors in parallel edit different elements of
# `.vectors`. Merge vector-by-vector against %O; conflict only when the same
# field of the same vector was changed differently on both sides, or when the
# vector list's id sequence changed.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

base=$1
ours=$2
theirs=$3

jq -e --slurp --indent 2 '
  .[0] as $b | .[1] as $o | .[2] as $t |
  if ($b | type) != "object" or ($o | type) != "object" or ($t | type) != "object" then error("not object")
  elif ($b.vectors | type) != "array" or ($o.vectors | type) != "array" or ($t.vectors | type) != "array" then error("vectors not array")
  elif ($o.vectors | map(.id)) != ($t.vectors | map(.id)) then error("vector id list changed")
  else
    ($b.vectors | map({key: .id, value: .}) | from_entries) as $bmap |
    ($t.vectors | map({key: .id, value: .}) | from_entries) as $tmap |
    $o | .vectors = [
      $o.vectors[] | . as $ov |
      ($tmap[$ov.id]) as $tv |
      ($bmap[$ov.id]) as $bv |
      if $ov == $tv or $tv == $bv then $ov
      elif $ov == $bv then $tv
      elif $bv == null then error("conflict on new vector")
      else
        reduce (($ov | keys_unsorted) + ($tv | keys_unsorted) | unique)[] as $k (
          {};
          if $ov[$k] == $tv[$k] or $tv[$k] == $bv[$k] then . + {($k): $ov[$k]}
          elif $ov[$k] == $bv[$k] then . + {($k): $tv[$k]}
          else error("field conflict on \($ov.id).\($k)")
          end
        )
      end
    ]
  end
' "$base" "$ours" "$theirs" > "$ours.tmp" || {
  rm -f "$ours.tmp"
  exit 1
}
mv "$ours.tmp" "$ours"
