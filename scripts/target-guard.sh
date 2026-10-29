#!/usr/bin/env bash
# Keep a shared cargo target honest across worktrees.
#
#   scripts/target-guard.sh --run CMD [ARG...]  guard, then run CMD while still
#                                               holding the target; exits with
#                                               CMD's status
#   scripts/target-guard.sh --hold PID          guard, and keep holding the target
#                                               for process PID (a script that
#                                               runs several cargo commands)
#   scripts/target-guard.sh --release PID       let go of what --hold PID took
#   scripts/target-guard.sh                     guard alone; a cargo run after it
#                                               is not held (prefer --run)
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
# A stamp per workspace member records the content every build of that member
# in the target was made from: its committed tree, any uncommitted change, and
# the files outside its directory it compiles in (an `include_str!` of config/
# or deploy/, a build script's .agents/ ledgers). Where this worktree's content
# matches, every build of the member in the target is this content and nothing
# is touched, so the cache stays shared. Where it differs, or no stamp says
# what the target holds, the member's cargo fingerprints are removed in every
# profile, so the next build of it, from any worktree, compiles that
# worktree's own sources.
#
# This script used to touch this worktree's files instead and stamp at once.
# That made only *this* worktree rebuild, and the stamp then named content the
# target did not hold whenever the build that followed skipped the member (a
# `sync`, a gate scoped to other crates): the next worktree with that content
# trusted the stamp and linked a sibling's build. A stamp that only matched
# its own content also missed the inputs outside the member's directory: two
# worktrees with one crate tree and different deploy/harness/prices.json
# shared one price table.
#
# The guard and the build after it must also be one step. Two gates
# interleaving (A guards, B guards, A builds, B builds) left B linking A's
# code under B's stamp, and every later guard of B saw its own stamp and kept
# it. So the guard locks the target (a directory: mkdir is atomic everywhere,
# Git Bash included) and --run and --hold keep the lock through the cargo
# commands that follow. A holder that died is found by its pid and its lock
# taken over.
#
# Registry crates are never affected: their builds are keyed by version, not by
# worktree. Cost: a few git calls in all, whatever the number of members (a
# process per member was 100 s on Windows at 214 members); only a member with
# uncommitted changes starts more. Quiet, and it never fails a caller.
set -uo pipefail

# Git for Windows' own usr/bin/bash.exe, started from PowerShell or the desktop
# app, has no /usr/bin on PATH: mkdir and sed are missing and find is
# System32's, and this script stopped at its first mkdir, silently, every time.
case "${OSTYPE:-}" in
msys* | cygwin*)
    case ":$PATH:" in
    *:/usr/bin:*) ;;
    *) PATH=/usr/bin:$PATH ;;
    esac
    ;;
esac

mode=guard
owner=$$
case "${1:-}" in
--run)
    shift
    [ "${1:-}" != -- ] || shift
    mode=run
    if [ $# -eq 0 ]; then
        echo 'usage: target-guard.sh --run CMD [ARG...]' >&2
        exit 2
    fi
    ;;
--hold | --release)
    mode=${1#--}
    owner=${2:-}
    case "$owner" in
    '' | *[!0-9]*)
        echo "usage: target-guard.sh $1 PID" >&2
        exit 2
        ;;
    esac
    ;;
'') ;;
*)
    echo 'usage: target-guard.sh [--run CMD [ARG...] | --hold PID | --release PID]' >&2
    exit 2
    ;;
esac

# The target cargo builds into: CARGO_TARGET_DIR when set (relative to the
# directory cargo runs in), else the worktree's target/ (the shared link).
root=$(git rev-parse --show-toplevel 2>/dev/null) || root=
target=
if [ -n "$root" ]; then
    case "${CARGO_TARGET_DIR:-}" in
    '') target=$root/target ;;
    /* | [A-Za-z]:[\\/]*) target=$CARGO_TARGET_DIR ;;
    *) target=$PWD/$CARGO_TARGET_DIR ;;
    esac
    # A Windows path's backslashes would be glob escapes below.
    target=${target//\\//}
    [ -e "$target" ] || target=
fi

# The guard needs bash 4.2: associative arrays (`local -A`) and `printf
# '%(%s)T'`. macOS's /bin/bash is 3.2, and it is the `bash` a Mac without
# Homebrew's finds: there `${is_member[.]}` is an arithmetic error that
# discards the whole `case "$mode"` below, so `--run` exited 1 without running
# its command, and `tasks close` read every cargo acceptance as not done. An
# older bash runs the command unguarded, as a host with no target to guard does.
if [ "${BASH_VERSINFO[0]:-0}" -lt 4 ] ||
    { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]:-0}" -lt 2 ]; }; then
    if [ -n "$target" ] && [ "$mode" != release ]; then
        echo "target-guard: bash ${BASH_VERSION:-?} cannot guard the shared target (4.2+ can); running unguarded" >&2
    fi
    [ "$mode" != run ] || exec "$@"
    exit 0
fi
state=${target:+$target/.susi-guard}
lock=${state:+$state/lock}
root_paths="src build.rs Cargo.toml tests benches examples"

# --- the lock ---------------------------------------------------------------
# $lock/owner is "<pid> <since> <ostype>". A pid is only meaningful in the
# runtime that wrote it (an MSYS pid is not a Windows one), so a holder from
# another runtime is presumed alive until SUSI_TARGET_LOCK_STALE seconds.
held=0
holder_alive() {
    local pid=$1 os=$2
    [ -n "$pid" ] || return 1
    [ "$os" = "${OSTYPE:-}" ] || return 0
    kill -0 "$pid" 2>/dev/null || [ -d "/proc/$pid" ]
}

acquire() {
    [ -n "$lock" ] || return 0
    [ -d "$state" ] || mkdir -p "$state" 2>/dev/null || return 0
    local line again pid since os now said=0 gone=0
    while ! mkdir "$lock" 2>/dev/null; do
        if [ ! -d "$lock" ]; then
            # Not held, yet it cannot be taken (a read-only target): build
            # unheld rather than wait forever.
            gone=$((gone + 1))
            [ "$gone" -lt 5 ] || return 0
            continue
        fi
        line=
        read -r line <"$lock/owner" 2>/dev/null
        read -r pid since os <<<"$line"
        case "${since:-}" in '' | *[!0-9]*) since=0 ;; esac
        if [ -n "$line" ] && [ "$pid" = "$owner" ]; then
            return 0 # this owner already holds it (--hold again)
        fi
        # An outer --run or --hold of this process tree holds it for us.
        if [ -n "$line" ] && [ "$pid" = "${SUSI_TARGET_GUARD_HELD:-}" ] && holder_alive "$pid" "$os"; then
            return 0
        fi
        printf -v now '%(%s)T' -1
        if [ -z "$line" ]; then
            # Taken, not yet signed: a taker that stays unsigned for a minute
            # died between its mkdir and its write.
            if [ -n "$(find "$lock" -prune -mmin +1 2>/dev/null)" ]; then
                rmdir "$lock" 2>/dev/null
                continue
            fi
        elif ! holder_alive "$pid" "$os" || [ $((now - since)) -ge "${SUSI_TARGET_LOCK_STALE:-21600}" ]; then
            # Its holder is gone. Take it over only if nobody else did meanwhile.
            again=
            read -r again <"$lock/owner" 2>/dev/null
            [ "$again" != "$line" ] || rm -rf "$lock"
            continue
        fi
        if [ "$said" = 0 ]; then
            echo "target-guard: waiting for the shared target ($lock, held by pid ${pid:-?})" >&2
            said=1
        fi
        sleep 1
    done
    printf -v now '%(%s)T' -1
    printf '%s %s %s\n' "$owner" "$now" "${OSTYPE:-}" >"$lock/owner"
    held=1
}

release() {
    [ -n "$lock" ] && [ -d "$lock" ] || return 0
    local line=
    read -r line <"$lock/owner" 2>/dev/null
    [ "${line%% *}" = "$owner" ] || return 0
    rm -rf "$lock"
}

# --- the guard --------------------------------------------------------------
guard() {
    [ -n "$state" ] || return 0
    local stamps=$state/stamps
    [ -d "$stamps" ] || mkdir -p "$stamps" 2>/dev/null || return 0
    # The first scheme's stamps recorded guard runs, not builds: never trust them.
    [ ! -d "$target/.susi-worktree-stamps" ] || rm -rf "$target/.susi-worktree-stamps" 2>/dev/null

    # Workspace members, as Cargo reads them: the list ends at the first `]`,
    # which may close the last member's own line (read to the end of the
    # file instead, the guard stamped "members" named 2021 and Muthu), a glob
    # member ("crates/*") names every directory under it with a Cargo.toml,
    # and a root manifest with a [package] is a member whether listed or not.
    # The root package is "." and owns $root_paths; every other member is a
    # directory.
    local listed m d
    listed=$(awk '
        /^[ \t]*\[package\][ \t]*(#.*)?$/ { print "." }
        /^[ \t]*members[ \t]*=[ \t]*\[/ && !done { inlist = 1 }
        inlist {
            code = $0; sub(/#.*/, "", code); rest = code
            while (match(rest, /"[^"]*"/)) {
                print substr(rest, RSTART + 1, RLENGTH - 2)
                rest = substr(rest, RSTART + RLENGTH)
            }
            if (index(code, "]")) { inlist = 0; done = 1 }
        }' "$root/Cargo.toml" 2>/dev/null)
    local -a names_listed=() members=() member_dirs=()
    local -A is_member=()
    set -f
    # shellcheck disable=SC2206 # split on whitespace, globbing off
    names_listed=($listed)
    set +f
    for m in "${names_listed[@]}"; do
        m=${m%/}
        if [ "$m" = . ]; then
            [ -n "${is_member[.]:-}" ] || {
                is_member[.]=1
                members+=(.)
            }
            continue
        fi
        # shellcheck disable=SC2231 # a glob member must expand, under $root
        for d in "$root"/$m; do
            [ -f "$d/Cargo.toml" ] || continue
            d=${d#"$root"/}
            [ -z "${is_member[$d]:-}" ] || continue
            is_member[$d]=1
            members+=("$d")
            member_dirs+=("$d")
        done
    done
    [ "${#members[@]}" -gt 0 ] || return 0
    local member_list
    member_list=$(printf '%s\n' "${members[@]}")

    # Files a member compiles in from outside its own directory. An
    # include_str!/include_bytes! names its file relative to the source file;
    # a build script names its inputs relative to its package, and a directory
    # it names ("../../.agents") is joined with the file names it names
    # ("identity.json"). Only tracked files count (an `ls-tree` below), and
    # the task queue (.agents/tasks/) is never a build input. Nor is a
    # directory: the bare "../../.agents" made `.agents` one, so a member with
    # uncommitted changes hashed every task-queue edit and untracked log under
    # it, and lost its build to each. An input's last component is a file
    # name, with an extension (a leading dot is not one: ".agents").
    # Out: "<member>\t<path>", in byte order.
    local inputs
    inputs=$({
        git -C "$root" grep -I -o -E '"\.\./[^"]*"' -- '*.rs' 2>/dev/null
        git -C "$root" grep -I -o -E '"[A-Za-z0-9_.][A-Za-z0-9_./-]*"' -- build.rs '*/build.rs' 2>/dev/null
    } | LC_ALL=C awk -v members="$member_list" -v rootpaths="$root_paths" '
        function norm(p,   n, part, out, k, i, r) {
            n = split(p, part, "/"); k = 0
            for (i = 1; i <= n; i++) {
                if (part[i] == "" || part[i] == ".") continue
                if (part[i] == "..") { if (k == 0) return ""; k--; continue }
                out[++k] = part[i]
            }
            r = ""
            for (i = 1; i <= k; i++) r = r (i > 1 ? "/" : "") out[i]
            return r
        }
        function dirof(f) { if (!index(f, "/")) return ""; sub(/\/[^\/]*$/, "", f); return f }
        function join(d, l) { return d == "" ? l : d "/" l }
        function owner(path,   top, best, i) {
            top = path; sub(/\/.*/, "", top)
            if (isroot[top] || isroot[path]) return "."
            best = ""
            for (i = 1; i <= m; i++)
                if (mem[i] != "." && (path == mem[i] || index(path, mem[i] "/") == 1) && length(mem[i]) > length(best)) best = mem[i]
            return best
        }
        function isfile(p,   b) {
            b = p; sub(/.*\//, "", b); sub(/^\./, "", b)
            return b ~ /^[^.][^\/]*\.[A-Za-z0-9]+$/
        }
        function want(own, d, lit,   c) {
            c = norm(join(d, lit))
            if (c != "" && isfile(c) && c !~ /^\.agents\/tasks(\/|$)/ && owner(c) != own) cand[own "\t" c] = 1
        }
        BEGIN {
            n = split(members, all, "\n"); m = 0
            for (i = 1; i <= n; i++) if (all[i] != "") mem[++m] = all[i]
            n = split(rootpaths, rp, " "); for (i = 1; i <= n; i++) isroot[rp[i]] = 1
        }
        {
            i = index($0, ":\""); if (!i) next
            f = substr($0, 1, i - 1); lit = substr($0, i + 2); sub(/"$/, "", lit)
            own = owner(f); if (own == "") next
            if (f == "build.rs" || f ~ /\/build\.rs$/) {
                if (!((f, lit) in seenlit)) { seenlit[f, lit] = 1; lits[f] = lits[f] "\n" lit }
                bown[f] = own
                next
            }
            want(own, dirof(f), lit)
        }
        END {
            for (f in bown) {
                own = bown[f]; d = dirof(f)
                k = split(substr(lits[f], 2), L, "\n")
                for (a = 1; a <= k; a++) {
                    want(own, d, L[a])
                    # A directory it names, joined with each name it names.
                    if (L[a] ~ /^\.\.?\// && !isfile(L[a])) for (b = 1; b <= k; b++) want(own, join(d, L[a]), L[b])
                }
            }
            # Byte order, so the identity reads the same in every worktree.
            n = 0
            for (key in cand) {
                for (j = ++n; j > 1 && sorted[j - 1] > key; j--) sorted[j] = sorted[j - 1]
                sorted[j] = key
            }
            for (j = 1; j <= n; j++) print sorted[j]
        }')
    local -a input_paths=()
    local -A seen_input=()
    local member path
    while IFS=$'\t' read -r member path; do
        [ -n "$path" ] || continue
        [ -z "${seen_input[$path]:-}" ] || continue
        seen_input[$path]=1
        input_paths+=("$path")
    done <<<"$inputs"

    # One pass: the committed identity of every member (its tree hashes plus
    # its inputs' blob hashes), whether anything of it is uncommitted, and the
    # inputs a dirty member's diff must cover.
    # Out: "<member>\t<committed-identity>\t<dirty 0|1>\t<inputs>".
    local lines
    # shellcheck disable=SC2086 # one argument per root path
    lines=$({
        git -C "$root" ls-tree HEAD -- "${member_dirs[@]}" $root_paths 2>/dev/null
        echo @inputs
        [ "${#input_paths[@]}" -eq 0 ] || git -C "$root" ls-tree HEAD -- "${input_paths[@]}" 2>/dev/null
        echo @status
        git -C "$root" status --porcelain --untracked-files=all 2>/dev/null
    } | LC_ALL=C awk -v members="$member_list" -v inputs="$inputs" -v rootpaths="$root_paths" '
        BEGIN {
            n = split(rootpaths, rp, " "); for (i = 1; i <= n; i++) isroot[rp[i]] = 1
            n = split(members, all, "\n"); m = 0
            for (i = 1; i <= n; i++) if (all[i] != "") order[++m] = all[i]
            n = split(inputs, lines, "\n")
            for (i = 1; i <= n; i++) {
                if (split(lines[i], mi, "\t") < 2) continue
                uses[mi[1]] = uses[mi[1]] " " mi[2]
                users[mi[2]] = users[mi[2]] " " mi[1]
            }
            part = "tree"
        }
        $0 == "@inputs" { part = "inputs"; next }
        $0 == "@status" { part = "status"; next }
        part == "tree" {
            owner = isroot[$4] ? "." : $4
            ident[owner] = ident[owner] $4 ":" $3 ";"
            next
        }
        part == "inputs" { if ($2 == "blob") blob[$4] = $3; next }
        {
            path = substr($0, 4); sub(/.* -> /, "", path)
            k = split(users[path], u, " "); for (i = 1; i <= k; i++) dirty[u[i]] = 1
            top = path; sub(/\/.*/, "", top)
            if (isroot[top] || isroot[path]) { dirty["."] = 1; next }
            # The member owning this path is the longest member prefix.
            best = ""
            for (i = 1; i <= m; i++) {
                mem = order[i]
                if (mem != "." && (path == mem || index(path, mem "/") == 1) && length(mem) > length(best)) best = mem
            }
            if (best != "") dirty[best] = 1
        }
        END {
            for (i = 1; i <= m; i++) {
                mem = order[i]; id = ident[mem]
                k = split(uses[mem], u, " ")
                for (j = 1; j <= k; j++) if (u[j] in blob) id = id u[j] ":" blob[u[j]] ";"
                print mem "\t" (id == "" ? "-" : id) "\t" ((mem in dirty) ? 1 : 0) "\t" substr(uses[mem], 2)
            }
        }')

    local -a stale=() stale_ids=() manifests=()
    local committed dirty uses current paths extra name last
    while IFS=$'\t' read -r member committed dirty uses; do
        [ -n "$member" ] || continue
        current=$committed
        if [ "$dirty" = 1 ]; then
            # Only a member with uncommitted changes pays for a diff.
            if [ "$member" = . ]; then paths=$root_paths; else paths=$member; fi
            # shellcheck disable=SC2086
            extra=$({
                git -C "$root" diff HEAD -- $paths $uses 2>/dev/null
                git -C "$root" ls-files --others --exclude-standard -- $paths $uses 2>/dev/null |
                    while IFS= read -r f; do git -C "$root" hash-object -- "$f" 2>/dev/null; done
            } | git hash-object --stdin 2>/dev/null)
            current="$committed+$extra"
        fi
        name=${member//\//_}
        name=${name//./_}
        last=
        [ ! -f "$stamps/$name" ] || read -r last <"$stamps/$name"
        [ "$last" != "$current" ] || continue
        stale+=("$name")
        stale_ids+=("$current")
        if [ "$member" = . ]; then manifests+=("$root/Cargo.toml"); else manifests+=("$root/$member/Cargo.toml"); fi
    done <<<"$lines"
    [ "${#stale[@]}" -gt 0 ] || return 0

    # The stale members' package names, which name their fingerprint
    # directories: "<package>-<16 hex>", one per unit (lib, test, build
    # script, check or build) in each profile and target triple.
    local names
    names=$(awk '
        FNR == 1 { inpkg = 0; got = 0 }
        /^[ \t]*\[/ { inpkg = ($0 ~ /^[ \t]*\[package\][ \t]*(#.*)?$/); next }
        inpkg && !got && /^[ \t]*name[ \t]*=/ {
            v = $0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); print v; got = 1
        }' "${manifests[@]}" 2>/dev/null)
    local -a doomed=()
    local fp entry
    for fp in "$target"/*/.fingerprint "$target"/*/*/.fingerprint; do
        [ -d "$fp" ] || continue
        while IFS= read -r entry; do
            [ -n "$entry" ] && doomed+=("$fp/$entry")
        done < <(ls -1 "$fp" 2>/dev/null | awk -v names="$names" '
            BEGIN { n = split(names, a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") want[a[i]] = 1 }
            match($0, /-[0-9a-f]+$/) && RLENGTH == 17 && (substr($0, 1, RSTART - 1) in want)')
    done
    if [ "${#doomed[@]}" -gt 0 ]; then
        # A build that could not be removed is still there: stamp nothing, so
        # the next guard tries again rather than vouching for it.
        rm -rf -- "${doomed[@]}" 2>/dev/null || return 0
        echo "target-guard: ${#stale[@]} workspace crate(s) were built from other content (or content no stamp records); they rebuild for this worktree" >&2
    fi
    local i
    for i in "${!stale[@]}"; do
        printf '%s\n' "${stale_ids[$i]}" >"$stamps/${stale[$i]}" 2>/dev/null
    done
    return 0
}

case "$mode" in
release)
    release
    exit 0
    ;;
hold)
    acquire
    guard
    exit 0
    ;;
run)
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap '[ "$held" = 0 ] || release' EXIT
    acquire
    guard
    # The command is held: a target-guard it runs (a nested gate or close)
    # sees its ancestor's lock rather than waiting on it forever.
    SUSI_TARGET_GUARD_HELD=$owner "$@"
    exit $?
    ;;
*)
    trap '[ "$held" = 0 ] || release' EXIT
    acquire
    guard
    exit 0
    ;;
esac
