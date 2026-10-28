#!/usr/bin/env bash
# Keep a shared cargo target honest across worktrees.
#
#   scripts/target-guard.sh     (run before cargo, in the worktree that builds)
#
# Worktrees share one `target/` (scripts/link-shared-target.sh), and cargo
# names a workspace crate's build by its path *relative to the workspace*, so
# `crates/susi-harness` is the same unit in every worktree. Cargo calls a unit
# fresh when its source files are older than its last build, from whichever
# worktree that build came from. So when another agent's worktree built a
# crate it had changed, this worktree's older, unchanged copy of the crate
# reads as fresh, and cargo links the other agent's code into this build:
# tests pass or fail against work that is not this branch's. (Seen: the
# harness branch would not compile because the workflow branch, which lacks
# `susi_harness::fleet`, had built susi-harness last.)
#
# This script records, per workspace member, the content this worktree has
# (its committed tree plus any uncommitted change) and the content last built
# into the shared target. Where they differ, it bumps the member's file times
# so cargo rebuilds it; where they match, the shared build is this worktree's
# own and nothing is touched, so the cache stays shared. Registry crates are
# never affected: their builds are keyed by version, not by worktree.
#
# Cost: a few git calls in all, whatever the number of members (a process per
# member was 100 s on Windows at 214 members); only a member with uncommitted
# changes, or one that must be rebuilt, starts more. Quiet, idempotent, and it
# never fails a caller.
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
target=$root/target
[ -e "$target" ] || exit 0
stamps=$target/.susi-worktree-stamps
mkdir -p "$stamps" 2>/dev/null || exit 0

# Workspace members, as Cargo.toml lists them. The root package is "." and owns
# the paths below; every other member is one directory.
members=$(sed -n '/^members = \[/,/^\]/p' "$root/Cargo.toml" | grep -oE '"[^"]+"' | tr -d '"')
[ -n "$members" ] || exit 0
root_paths="src build.rs Cargo.toml tests benches examples"

# One pass: committed hashes of every member's paths, and which members have
# uncommitted changes. Out: "<member> <committed-identity> <dirty 0|1>".
# shellcheck disable=SC2086 # one argument per member path is the point
lines=$(
    {
        printf '%s\n' "$members" | sed 's/^/M /'
        git -C "$root" ls-tree HEAD -- $(printf '%s\n' "$members" | grep -vx '\.') $root_paths 2>/dev/null |
            awk '{ print "T", $4, $3 }'
        git -C "$root" status --porcelain --untracked-files=all 2>/dev/null |
            cut -c4- | sed 's/.* -> //; s/^/C /'
    } | awk -v rootpaths="$root_paths" '
        BEGIN { n = split(rootpaths, rp, " "); for (i = 1; i <= n; i++) isroot[rp[i]] = 1 }
        $1 == "M" { order[++m] = $2; next }
        $1 == "T" {
            owner = isroot[$2] ? "." : $2
            ident[owner] = ident[owner] $2 ":" $3 ";"
            next
        }
        $1 == "C" {
            path = $2
            top = path; sub(/\/.*/, "", top)
            if (isroot[top] || isroot[path]) { dirty["."] = 1; next }
            # The member owning this path is the longest member prefix.
            best = ""
            for (i = 1; i <= m; i++) {
                mem = order[i]
                if (mem != "." && (path == mem || index(path, mem "/") == 1) && length(mem) > length(best)) best = mem
            }
            if (best != "") dirty[best] = 1
            next
        }
        END { for (i = 1; i <= m; i++) { mem = order[i]; print mem, (ident[mem] == "" ? "-" : ident[mem]), (mem in dirty) ? 1 : 0 } }
    '
)

rebuilt=0
while read -r member committed dirty; do
    [ -n "$member" ] || continue
    current=$committed
    if [ "$dirty" = 1 ]; then
        # Only a member with uncommitted changes pays for a diff.
        if [ "$member" = . ]; then paths=$root_paths; else paths=$member; fi
        # shellcheck disable=SC2086
        extra=$({
            git -C "$root" diff HEAD -- $paths 2>/dev/null
            git -C "$root" ls-files --others --exclude-standard -- $paths 2>/dev/null |
                while IFS= read -r f; do git -C "$root" hash-object -- "$f" 2>/dev/null; done
        } | git hash-object --stdin 2>/dev/null)
        current="$committed+$extra"
    fi
    name=${member//\//_}
    stamp=$stamps/${name//./_}
    last=
    [ -f "$stamp" ] && read -r last <"$stamp"
    [ "$last" = "$current" ] && continue
    if [ -n "$last" ]; then
        # Built before, from other content: make cargo see this worktree's.
        if [ "$member" = . ]; then
            for p in $root_paths; do
                [ -e "$root/$p" ] && find "$root/$p" -type f -exec touch {} + 2>/dev/null
            done
        else
            find "$root/$member" -type f -exec touch {} + 2>/dev/null
        fi
        rebuilt=$((rebuilt + 1))
    fi
    printf '%s\n' "$current" >"$stamp" 2>/dev/null
done <<<"$lines"
[ "$rebuilt" -eq 0 ] || echo "target-guard: $rebuilt workspace crate(s) differ from the shared build; they rebuild for this worktree" >&2
exit 0
