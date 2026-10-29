#!/usr/bin/env bash
# Give a worktree its own cargo target, warm from a snapshot of origin/main.
#
#   scripts/seed-target.sh [DIR]               seed DIR's target/ (default: this
#                                              checkout) before anything builds
#   scripts/seed-target.sh --background [DIR]  replace a link now, copy in the
#                                              background (the session-start,
#                                              post-merge, post-commit and
#                                              post-checkout hooks)
#   scripts/seed-target.sh --refresh [--force] [--log FILE]
#                                              rebuild and republish the snapshot
#                                              (the primary watcher runs this)
#
# Worktrees used to share the primary checkout's target/ through a link. Cargo
# names a workspace crate's build by its path relative to the workspace, so a
# crate another worktree built last read as fresh in this one and its code was
# linked into this branch's build; scripts/target-guard.sh narrowed that but
# could not close it (a cargo outside the guard, the harness, an IDE's check,
# test binaries that bake another worktree's CARGO_MANIFEST_DIR, two Git
# installs' pid tables), and every build on the host queued behind one lock.
# A target of the worktree's own is correct by construction. Cold it costs
# ~645 s; seeded it costs the copy of the snapshot's registry crates (see
# below) plus the ~80 workspace crates (240-390 s), and it builds alongside
# every other worktree instead of after them.
#
# In the primary checkout it changes nothing: its target/ is the old shared
# one, which worktrees made before the seeding still link to and build into
# until each next merges origin/main (and so gets its own). Cargo's lock keeps
# their builds and the primary's intact, not apart: a crate one of them built
# last can read as fresh to a cargo in the primary. So there it says when any
# worktree still links that target, and the agent docs have an agent then use
# an installed susi, or skip `workflow check` (scripts/susi-worktree.sh runs
# no cargo).
#
# What it does to DIR/target (never in the primary checkout):
#   1. A link there (a symlink, or a junction on Windows) is removed: the link
#      only, never what it points to.
#   2. A missing or empty target is filled from the snapshot, SUSI_TARGET_SEED
#      (default <primary>/../.susi-target-seed), timestamps kept: robocopy on
#      Windows, `cp -a --reflink=auto` elsewhere. Never hard links: cargo
#      rewrites fingerprint files in place, and a link would let one worktree's
#      build rewrite another's. incremental/ is left out, and nothing is ever
#      removed through a link the copy holds: every delete in this script
#      checks each directory from the tree it works in down to what it
#      removes. Only a snapshot --refresh published is copied (it carries
#      .susi-seed): never a checkout, never the primary's own target, never
#      one with a link where cargo keeps builds (a profile, a target triple's
#      directory, a profile's fingerprints), which cargo never makes.
#   3. Every unit not built from a registry or git source of DIR's Cargo.lock
#      is stripped from the copy: the workspace members, the path dependencies
#      (vendor/reqwest, vendor/esaxx-rs: Cargo.lock lists them with no
#      source), and any crate that lockfile does not name. Their builds are
#      named by their path relative to the workspace, so the snapshot's would
#      read as fresh wherever this checkout's files are older than the
#      snapshot, and its test binaries carry the snapshot checkout's
#      CARGO_MANIFEST_DIR. They rebuild from this worktree's own sources;
#      registry crates, keyed by version, features and compiler, stay warm.
#      --refresh strips the snapshot the same way, so this is a check more
#      than work; but a copy it cannot strip all the way (a lockfile it
#      cannot read, a stripped unit's fingerprint or file still there after,
#      a link where cargo keeps builds) is dropped, and the target starts
#      empty: cold, and correct.
# The copy is made in DIR/target.seed-<pid>-<time>.tmp (ignored as *.tmp) and
# renamed into place, so a cargo that starts meanwhile never reads half a
# snapshot: it makes its own target/, and the copy is dropped. A worktree
# removed while its copy runs (`git worktree remove`) is never brought back:
# nothing is made once DIR/.git or its git dir is gone, never with its missing
# parents, and the copy tool (robocopy makes its destination's parents) leaves
# only empty directories, which are taken back. Left behind, such a directory
# makes the next `workflow start` or scripts/susi-worktree.sh of that name
# refuse it as "an unrelated checkout". The worktree is checked again right
# after the rename, and a target renamed into a worktree being removed is
# taken back. What is left is a removal whose delete started before the rename
# and has not yet reached DIR/.git when that check runs (milliseconds): git
# then reports DIR as not empty, and DIR/target stays, to be removed by hand.
#
# Nothing is copied, and no copy is waited for, when CARGO_TARGET_DIR (or
# CARGO_BUILD_TARGET_DIR) names a directory other than DIR/target: the cargo
# the caller runs next builds there. A link is still replaced.
#
# The snapshot holds only registry and git crates: no workspace build, no
# binaries a direct exec would mistake for this branch's (target/debug/susi,
# deps/susi.exe), no examples, no build-script output of a path package. So it
# does not grow with each release: a version bump renames most workspace
# units, and those are never in it. Measured on a 28-thread Windows host from
# a 9.9 GiB gate target (7.5 GiB, 12.6k files without incremental/): the strip
# took 6 s and left 5.1 GiB in 10.5k files, which robocopy copies in about
# 17 s (a whole target took 64-72 s). That was a gate's target, not a
# published snapshot: the snapshot is the registry crates of a whole-workspace
# `cargo build` plus `cargo clippy` (--all-targets), so expect it larger and
# its copy longer, up to about the whole target's minute; measure it once the
# watcher has published one.
#
# The vendored path crates cost every new worktree: cargo checks a path
# dependency by its files' timestamps and a fresh checkout writes every file
# new, so vendor/reqwest and vendor/esaxx-rs recompile (they are stripped here
# anyway), and so does everything above them: ra2a, rmcp, tokenizers,
# fastembed, the candle crates. Measured on the same host, a seeded worktree's
# first `cargo build --workspace --all-targets` took 341-389 s with the path
# patches and 240 s with those crates as registry sources: some 100-150 s
# (about 1490 unit-seconds) per worktree. Pointing [patch.crates-io] at a
# pinned git revision of a fork instead of `path = "vendor/..."` would keep
# them warm in the snapshot; that needs a fork to host the two patches, and is
# not done.
#
# A target/ that holds anything is this worktree's own build and is left alone,
# so running this again changes nothing: every path that runs cargo for an
# agent calls it first (the finish gate and sync in parallel-workflow.sh, the
# pre-push hook, `tasks close`; and for the installed susi, v0.21.23, the
# retired target-guard.sh its `tasks close` runs and link-shared-target.sh its
# harness runs, whatever slashes the path they are run by has), so a worktree
# still linked to the old shared target gets its own before it builds. The
# git hooks call it too, in the background, whenever target/ is a link:
# post-merge (a `git merge origin/main` is what brings this script to a
# worktree made before it); post-commit, because Git runs no post-merge for a
# merge that stops on conflicts, and the commit that concludes it (`git
# commit`, `git merge --continue`) is the first hook to run after; and
# post-checkout (a rebase, a `pull --rebase`), which also runs it when `git
# worktree add` has just made the checkout, so even v0.21.23's `workflow
# start`, which links a target/ that does not exist yet, finds a real one.
# A foreground run whose
# worktree is being copied into already waits for that copy, which is why the
# agent docs have an agent run this once, in the foreground, before its first
# plain cargo in a worktree just made (a hook's background copy may still
# run); `tasks close` and the retired guard call it for an empty or missing
# target too, not only a link. SUSI_NO_SEED=1
# skips the copy (a link is still replaced): the first build is cold. With no
# snapshot the target is a plain empty one, which is just as correct. It
# never fails its caller.
#
# --refresh is the snapshot's only writer. It builds origin/main (`cargo build`
# and `cargo clippy`, --workspace --all-targets --locked) at low priority in a
# clone of its own, SUSI_TARGET_SEED_SRC (default <seed>-src): one this script
# made, marked .git/susi-seed-src, and never the primary checkout or a
# worktree of it, whose branch and files belong to whoever works there. The
# snapshot and that clone are dot-named beside the primary checkout, out of
# the `<primary>-<name>` names scripts/susi-worktree.sh and `workflow start`
# give worktrees: a worktree named `target-seed` once sat exactly where the
# snapshot was looked for, and was copied into every new target. That
# clone's target only grows, so it starts over when Cargo.lock, the toolchain
# file or rustc changes, or once it is twice its size after its last fresh
# build. One refresh runs at a time (<seed>.lock); each stages in a directory
# of its own (<seed>.new.<pid>-<time>), strips it, marks it, checks the marker
# and that the staging is still its own, and publishes by rename. It does
# nothing when the snapshot already holds origin/main, or is younger than
# SUSI_SEED_REFRESH_SECS (3600) unless --force, and it replaces only a snapshot
# it made itself (one carrying .susi-seed).
#
# Its locks are directories holding their holder's pid, start time, Windows
# pid and runtime. A pid means something only in the process table of the
# runtime that wrote it, and a host can have two (Program Files' Git and
# scoop's): a holder of this runtime is alive while its pid is, one of another
# is looked up by its Windows pid, and only a holder neither can tell about is
# presumed alive until the lock is SUSI_SEED_LOCK_STALE seconds old (a day for
# --refresh, two hours for a worktree's copy). A live holder's lock is never
# taken over, however long it builds. No flock(1) is needed, so macOS and Git
# Bash lock alike.

# Before anything else, on Git for Windows (MSYS) or Cygwin: /usr/bin first on
# PATH, whatever PATH holds. A raw usr/bin shell started from PowerShell, the
# desktop app or a harness's `sh` has none of it on PATH, or has it after
# System32, where `bash` is WSL's and `find` is Windows' own (whose answer to
# `-mmin` is empty, so a stale lock would never be taken over).
case "${OSTYPE:-}" in
msys* | cygwin*)
    PATH=/usr/bin:$PATH
    export PATH
    ;;
esac

# A bash script, written for bash 3.2 (macOS's) and up. Bash in its POSIX mode
# (started as `sh`: Git for Windows', macOS's) leaves that mode, in place: no
# second bash is looked up. A `sh` that is not bash (dash) runs this under
# bash, once; with no bash to be had it does nothing.
if [ -n "${BASH_VERSION:-}" ]; then
    ! shopt -oq posix 2>/dev/null || set +o posix
else
    if [ -z "${SUSI_SEED_BASH:-}" ] && [ -f "$0" ] && command -v bash >/dev/null 2>&1; then
        SUSI_SEED_BASH=1 exec bash "$0" "$@"
    fi
    exit 0
fi
set -uo pipefail

# This script's own path, which the background copy runs again. On Windows a
# caller may name it the way Rust joins a path (`F:/w\scripts\seed-target.sh`):
# its backslashes are made slashes before the name is split off.
me=${BASH_SOURCE[0]}
case "${OSTYPE:-}" in msys* | cygwin*) me=${me//\\//} ;; esac
self=$(cd "$(dirname "$me")" 2>/dev/null && pwd)/${me##*/}

say() { printf 'target: %s\n' "$*" >&2; }

on_windows() {
    case "${OSTYPE:-}" in msys* | cygwin*) return 0 ;; esac
    return 1
}

winpath() { cygpath -aw "$1" 2>/dev/null || printf '%s' "$1"; }

no_seed() { [ "${SUSI_NO_SEED:-}" = 1 ]; }

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# The snapshot for the clone whose primary checkout is $1. Dot-named, so no
# worktree name puts a checkout there (worktrees are <primary>-<name>).
seed_dir() {
    local seed=${SUSI_TARGET_SEED:-$(dirname "$1")/.susi-target-seed}
    printf '%s' "${seed//\\//}"
}

# A symlink, or a junction: Git Bash reports both as links.
is_link() { [ -L "$1" ]; }

# Remove the link at $1 and nothing it points to. `rm` without -r unlinks a
# symlink or a junction itself. On Windows cmd's rmdir without /s, which also
# removes only a junction, is the fallback; the path is its own argument, as
# a quote inside cmd's command string reaches cmd escaped and is refused.
remove_link() {
    local link=$1
    is_link "$link" || return 1
    rm -f -- "$link" 2>/dev/null
    if is_link "$link" && on_windows; then
        MSYS2_ARG_CONV_EXCL='*' cmd.exe /d /c rmdir "$(winpath "$link")" >/dev/null 2>&1
    fi
    ! is_link "$link" && [ ! -e "$link" ]
}

# Whether the path $2, under the directory $1, is reached through a link: $2
# itself, or any directory between $1 and it, is one. A path not under $1
# counts as reached through one: it is refused, never deleted.
through_link() {
    local root=${1%/} at=$2 up
    case "$at" in "$root"/?*) ;; *) return 0 ;; esac
    while [ "$at" != "$root" ]; do
        is_link "$at" && return 0
        up=${at%/*}
        [ "$up" != "$at" ] || return 0
        at=$up
    done
    return 1
}

# Whether the build tree $1 holds a link where cargo keeps builds: an entry of
# its own (a profile, a target triple's directory), or an entry of one of its
# directories that is no profile (a target triple's profile). Cargo makes none
# there: its only links are what it copies up into a profile (a .dSYM
# directory on macOS), and a profile's .fingerprint, deps/ and build/ are
# checked on their own (through_link). A link there is another tree's builds,
# which a strip would delete through and cargo would build into, and one that
# leads to a tree with no fingerprints is seen by nothing else. tmp/ is the
# workspace's tests' scratch, removed whole, never through a link.
holds_foreign_link() {
    local tree=$1 a b
    for a in "$tree"/* "$tree"/.[!.]* "$tree"/..?*; do
        [ -e "$a" ] || [ -L "$a" ] || continue
        is_link "$a" && return 0
        [ -d "$a" ] && [ "${a##*/}" != tmp ] && [ ! -d "$a/.fingerprint" ] || continue
        for b in "$a"/* "$a"/.[!.]* "$a"/..?*; do
            ! is_link "$b" || return 0
        done
    done
    return 1
}

# Remove a directory this script made. Never through a link.
discard() {
    local dir=$1
    [ -n "$dir" ] || return 0
    if is_link "$dir"; then
        remove_link "$dir"
        return 0
    fi
    [ ! -d "$dir" ] || rm -rf -- "$dir" 2>/dev/null
    return 0
}

# Empty as far as a build goes: nothing, or only what cargo writes before it
# builds anything.
holds_nothing() {
    local entry
    for entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        case ${entry##*/} in
        CACHEDIR.TAG | .rustc_info.json) continue ;;
        esac
        return 1
    done
    return 0
}

# The directory $1 as it resolves, links followed: on Windows in its drive
# form (C:/...), lowercased, which no mount can disguise (Git Bash mounts the
# temp directory at /tmp, so one directory has two POSIX paths).
canon_dir() {
    local d
    if on_windows; then
        d=$(cd -P "$1" 2>/dev/null && pwd -W) || return 1
        lower "$d"
    else
        cd -P "$1" 2>/dev/null && pwd -P
    fi
}

# A snapshot to copy: one --refresh published (it carries .susi-seed, which
# nothing else writes), with a profile's fingerprints in it. Never a checkout:
# a worktree that sat where the snapshot is looked for once had its sources,
# its own target and its .git copied into every new target. Never the primary
# checkout's own target (the old shared one holds every worktree's builds, and
# is far too big to copy). Never one with a link where cargo keeps builds (a
# profile, a target triple's directory, a profile's fingerprints: one is
# reached through a link, or holds_foreign_link): those are another tree's
# builds, and a copy that keeps the link (`cp -a`) would reach through it.
usable_seed() {
    local seed=$1 primary=$2 real shared fp found=1
    [ -d "$seed" ] && [ -f "$seed/.susi-seed" ] || return 1
    [ ! -e "$seed/.git" ] && [ ! -L "$seed/.git" ] || return 1
    real=$(canon_dir "$seed") || return 1
    shared=$(canon_dir "$primary/target") || shared=
    [ "$real" != "$shared" ] || return 1
    ! holds_foreign_link "$seed" || return 1
    for fp in "$seed"/*/.fingerprint "$seed"/*/*/.fingerprint; do
        [ -e "$fp" ] || [ -L "$fp" ] || continue
        ! through_link "$seed" "$fp" || return 1
        [ ! -d "$fp" ] || found=0
    done
    return "$found"
}

# The directory the environment has cargo build into, printed when that is not
# the target $1: CARGO_TARGET_DIR, else CARGO_BUILD_TARGET_DIR (as cargo reads
# them; a relative one is taken from where cargo runs, which is where this
# runs). Fails when neither is set, or it names $1 itself, however spelled. A
# copy into $1 would then be read by no cargo that runs with this
# environment: every caller (`tasks close`, the retired guard's `--run`, the
# finish gate, `sync`, the pre-push hook) runs its cargo with the environment
# it runs this with.
builds_elsewhere() {
    local t=$1 named=${CARGO_TARGET_DIR:-${CARGO_BUILD_TARGET_DIR:-}} a= b=
    [ -n "$named" ] || return 1
    named=${named//\\//}
    case "$named" in
    /* | [A-Za-z]:/*) ;;
    *) named=$PWD/$named ;;
    esac
    while [ "${named%/}" != "$named" ] && [ -n "${named%/}" ]; do named=${named%/}; done
    if [ -d "$named" ] && [ -d "$t" ]; then
        a=$(canon_dir "$named") && b=$(canon_dir "$t") || a=
    elif a=$(canon_dir "$(dirname "$named")") && b=$(canon_dir "$(dirname "$t")"); then
        # One is not made yet: the same parent, and the same name.
        a=$a/$(basename "$named") b=$b/$(basename "$t")
        ! on_windows || a=$(lower "$a") b=$(lower "$b")
    else
        a=
    fi
    [ -n "$a" ] && [ "$a" = "$b" ] && return 1
    printf '%s' "$named"
}

# In the primary checkout $1, which is never seeded: say so when worktrees
# still link its target/ (made before the seeding; each gets its own at its
# next merge of origin/main, sync, finish gate or push). They build into it,
# and cargo's lock keeps those builds and this checkout's intact, not apart: a
# crate one of them built last can read as fresh to a cargo run here, and run
# in place of this checkout's. Nothing is changed.
note_shared_primary() {
    local primary=$1 shared line wt n=0 first=
    [ -d "$primary/target" ] && ! is_link "$primary/target" || return 0
    shared=$(canon_dir "$primary/target") || return 0
    while IFS= read -r line; do
        case "$line" in
        "worktree "*) wt=${line#worktree } ;;
        *) continue ;;
        esac
        is_link "$wt/target" && [ "$(canon_dir "$wt/target")" = "$shared" ] || continue
        n=$((n + 1))
        [ -n "$first" ] || first=$wt
    done < <(git -C "$primary" worktree list --porcelain 2>/dev/null)
    [ "$n" -gt 0 ] || return 0
    [ "$n" -lt 2 ] || first="$first, ..."
    say "$primary/target is still the shared target of $n worktree(s) made before the seeding ($first) until each next merges origin/main: a cargo here can run a crate one of them built in place of this checkout's (cargo's lock keeps the builds intact, not apart). Here, use an installed susi, or skip \`workflow check\`: scripts/susi-worktree.sh runs no cargo, and cargo is correct in the worktree it makes"
}

# Whether the worktree $1 (git dir $2) is still there. `git worktree remove`
# may run while a copy into it does: once it has, nothing is made there.
worktree_alive() {
    { [ -e "$1/.git" ] || [ -L "$1/.git" ]; } && [ -d "$2" ]
}

# Take back the empty directories a run into the removed worktree $1 may have
# left (its target/ $2, and $1 itself: robocopy makes its destination's
# parents). Only empty ones, only once $1/.git is gone, never a link.
leave_no_trace() {
    local dir=$1 t=$2
    [ ! -e "$dir/.git" ] && [ ! -L "$dir/.git" ] || return 0
    is_link "$t" || rmdir "$t" 2>/dev/null
    is_link "$dir" || rmdir "$dir" 2>/dev/null
    return 0
}

# An empty target/ $1 for the worktree $2 (git dir $3): what every path that
# copies nothing leaves (cold, and correct). Made only while the worktree is
# there, and never with its parents.
empty_target() {
    if worktree_alive "$2" "$3"; then
        [ -e "$1" ] || [ -L "$1" ] || mkdir "$1" 2>/dev/null
        worktree_alive "$2" "$3" || leave_no_trace "$2" "$1"
    else
        leave_no_trace "$2" "$1"
    fi
    return 0
}

# Rename $1 to the path $2, which must not exist: a plain `mv` onto a directory
# that appeared meanwhile would move $1 inside it.
rename_dir() {
    if mv --version >/dev/null 2>&1; then
        mv -T -- "$1" "$2" 2>/dev/null
    else
        [ ! -e "$2" ] && mv -- "$1" "$2" 2>/dev/null
    fi
}

# --- locks and owners that hold across Git installs --------------------------
# An owner line is "<pid> <since> <winpid> <runtime>". The runtime names the
# process table the pid belongs to: on Windows each Git install is an MSYS
# runtime with a table of its own, named by its install root (last on the
# line: it may hold spaces); elsewhere there is one table, named by OSTYPE.
my_runtime=
if on_windows; then
    my_runtime=$(cd / 2>/dev/null && pwd -W 2>/dev/null) || my_runtime=
    [ -n "$my_runtime" ] || my_runtime=$(cygpath -m / 2>/dev/null) || my_runtime=
    my_runtime=${my_runtime%/}
fi
[ -n "$my_runtime" ] || my_runtime=${OSTYPE:-unknown}
my_wpid=-
[ ! -r "/proc/$$/winpid" ] || read -r my_wpid 2>/dev/null <"/proc/$$/winpid" || my_wpid=-
case "$my_wpid" in '' | *[!0-9]*) my_wpid=- ;; esac
my_since=$(date +%s)
my_line="$$ $my_since $my_wpid $my_runtime"

same_runtime() { [ "$(lower "$1")" = "$(lower "$my_runtime")" ]; }

# Whether an owner line ($1 pid, $2 winpid, $3 runtime) is this process's.
is_me() {
    [ "$1" = "$$" ] && same_runtime "$3" &&
        { [ "$2" = - ] || [ "$my_wpid" = - ] || [ "$2" = "$my_wpid" ]; }
}

# Whether Windows process $1 runs a shell this runtime's table does not list:
# a holder from another Git install. 0: it does. 1: it does not. 2: there is
# no `ps -W` to ask. `ps -W` lists every Windows process; a row with a process
# group is one of this runtime's (a pid reused here: this runtime's own
# holders are found by their pid), and a row that is no shell is a pid reused
# by some other program. The command is the last field, which keeps the
# program's name whatever spaces its directory has.
winpid_state() {
    local out
    out=$(ps -W 2>/dev/null) || return 2
    [ -n "$out" ] || return 2
    printf '%s\n' "$out" | awk -v w="$1" '
        NR == 1 { next }
        { f = ($1 ~ /^[0-9]+$/) ? 1 : 2 }  # a status letter may lead
        $(f + 3) == w {
            found = 1
            if ($(f + 2) != 0) ours = 1
            else if (tolower($NF) ~ /(^|[\\\/])(ba)?sh(\.exe)?$/) shell = 1
        }
        END { exit !(found && !ours && shell) }'
}

# Whether the holder an owner line names ($1 pid, $2 since, $3 winpid, $4
# runtime) may still run. Age never ends a holder whose liveness can be told:
# a refresh at nice 19 on a busy host can build for hours, and taking its lock
# over would start a second build in the same checkout and target.
#   - This runtime's own: alive while its pid is (kill -0, or /proc), and
#     that pid still names the Windows process it recorded (a pid this
#     runtime handed out again is someone else); on Windows also while its
#     recorded Windows pid still runs a shell outside this runtime's table.
#   - Another runtime's, with a Windows pid recorded: alive while that pid
#     runs a shell (`ps -W`).
#   - Otherwise (no Windows pid, no `ps -W`): it cannot be told, so it counts
#     as alive until its line is $5 seconds old.
holder_alive() {
    local pid=$1 since=$2 wpid=$3 rt=$4 stale=$5 now_wpid
    case "$pid" in '' | *[!0-9]*) return 1 ;; esac
    case "$since" in '' | *[!0-9]*) since=0 ;; esac
    case "$wpid" in '' | *[!0-9]*) wpid=- ;; esac
    if same_runtime "$rt"; then
        if kill -0 "$pid" 2>/dev/null || [ -d "/proc/$pid" ]; then
            now_wpid=
            [ "$wpid" = - ] || [ ! -r "/proc/$pid/winpid" ] ||
                read -r now_wpid 2>/dev/null <"/proc/$pid/winpid"
            [ -z "$now_wpid" ] || [ "$now_wpid" = "$wpid" ]
            return
        fi
        [ "$wpid" != - ] && on_windows && winpid_state "$wpid"
        return
    fi
    if [ "$wpid" != - ] && on_windows; then
        winpid_state "$wpid"
        case $? in
        0) return 0 ;;
        1) return 1 ;;
        esac
    fi
    [ $(($(date +%s) - since)) -lt "$stale" ]
}

# Write this process's owner line into the file $1, by rename: a reader sees
# the old line or the new one, never half of one.
sign() {
    printf '%s %s %s %s\n' "$$" "$(date +%s)" "$my_wpid" "$my_runtime" >"$1.$$" 2>/dev/null &&
        mv -f -- "$1.$$" "$1" 2>/dev/null
}

# Take the lock directory $1 for this process without waiting; a holder is
# presumed gone once its line is $2 seconds old. 0: taken. 1: a live holder
# has it. 2: no lock can be made there.
#
# A gone holder's lock is never removed (a taker that judged it gone would
# remove whatever lock stood there when its rm ran, the next holder's
# included): each taker makes a token named after the line it judged gone,
# mkdir being atomic, and the one that makes it reads the line again and
# signs the lock in place. A token whose taker died before signing (a minute
# old) is passed by making the next one.
#
# Only the lock itself is made, never a missing parent: a worktree's lock is
# in its git dir, and a parent missing there is a worktree removed meanwhile,
# whose .git/worktrees/<name> must not come back. A caller whose lock's parent
# may legitimately be missing makes it first.
take_lock() {
    local lock=$1 stale=$2 line pid since wpid rt key k tok again
    if mkdir "$lock" 2>/dev/null; then
        sign "$lock/owner" && return 0
        rm -rf -- "$lock" 2>/dev/null
        return 2
    fi
    [ -d "$lock" ] && ! is_link "$lock" || return 2
    line=
    read -r line 2>/dev/null <"$lock/owner"
    if [ -z "$line" ]; then
        # Made and not yet signed; its maker died in between once a minute passes.
        [ -n "$(find "$lock" -prune -mmin +1 2>/dev/null)" ] || return 1
        key=unsigned
    else
        read -r pid since wpid rt <<<"$line"
        if is_me "$pid" "$wpid" "$rt"; then
            return 0
        fi
        holder_alive "$pid" "$since" "$wpid" "$rt" "$stale" && return 1
        key=$(printf '%s' "$pid.$since" | tr -c '0-9.' _)
    fi
    k=1
    while [ "$k" -le 5 ]; do
        tok=$lock/takeover.$key.$k
        if mkdir "$tok" 2>/dev/null; then
            again=
            read -r again 2>/dev/null <"$lock/owner"
            [ "$again" = "$line" ] || return 1
            sign "$lock/owner" && return 0
            return 1
        fi
        [ -d "$tok" ] || return 1
        [ -n "$(find "$tok" -prune -mmin +1 2>/dev/null)" ] || return 1
        k=$((k + 1))
    done
    return 1
}

# The lock this process holds (one at most: a seeding's or a refresh's).
held_lock=
# Let go of it: only while its line is this process's own.
drop_lock() {
    local lock=$held_lock line pid since wpid rt
    held_lock=
    [ -n "$lock" ] && [ -d "$lock" ] || return 0
    line=
    read -r line 2>/dev/null <"$lock/owner"
    read -r pid since wpid rt <<<"$line"
    is_me "$pid" "$wpid" "$rt" || return 0
    rm -rf -- "$lock" 2>/dev/null
    return 0
}
trap drop_lock EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Mark the staging directory $1 as this run's: a sweep by another run leaves
# it alone while this one lives.
claim_staging() { printf '%s\n' "$my_line" >"$1/.susi-seed-run" 2>/dev/null; }

# Whether the staging directory $1 is still this run's.
still_mine() {
    local line=
    read -r line 2>/dev/null <"$1/.susi-seed-run"
    [ "$line" = "$my_line" ]
}

# Remove the staging directories among $2... that no live run owns: each one
# carries its run's owner line (.susi-seed-run), counted gone after $1
# seconds; one with no line is from a run that died before it wrote one, once
# it is an hour old.
sweep_staging() {
    local stale=$1 d line pid since wpid rt
    shift
    for d in "$@"; do
        [ -e "$d" ] || [ -L "$d" ] || continue
        if is_link "$d"; then
            remove_link "$d"
            continue
        fi
        [ -d "$d" ] || continue
        line=
        read -r line 2>/dev/null <"$d/.susi-seed-run"
        if [ -n "$line" ]; then
            read -r pid since wpid rt <<<"$line"
            is_me "$pid" "$wpid" "$rt" && continue
            holder_alive "$pid" "$since" "$wpid" "$rt" "$stale" && continue
        else
            [ -n "$(find "$d" -prune -mmin +60 2>/dev/null)" ] || continue
        fi
        discard "$d"
    done
}

# --- copying and stripping a build tree --------------------------------------
# Copy the build tree $1 into the directory $2, which the caller made (never
# made here, nor its parents), timestamps kept, with no incremental/ caches,
# guard state or snapshot markers. robocopy's exit codes 0-7 all mean success
# (what it copied, skipped or found extra); 8 and up is a failure. robocopy
# skips junctions (/XJ); `cp -a` copies a link as a link, and drop_extras
# never reaches through one.
copy_tree() {
    local from=$1 to=$2 rc
    [ -d "$to" ] && ! is_link "$to" || return 1
    if on_windows && command -v robocopy >/dev/null 2>&1; then
        MSYS2_ARG_CONV_EXCL='*' robocopy "$(winpath "$from")" "$(winpath "$to")" \
            /E /COPY:DAT /DCOPY:DAT /MT:32 /XJ /R:1 /W:1 \
            /XD incremental .susi-guard .susi-strip /XF .susi-seed .susi-seeded .susi-seed-run \
            /NP /NFL /NDL /NJH /NJS >/dev/null 2>&1
        rc=$?
        [ "$rc" -lt 8 ]
        return
    fi
    if cp --version >/dev/null 2>&1; then
        cp -a --reflink=auto -- "$from/." "$to/" || return 1
    else
        # BSD cp (macOS): -p keeps the times, -P copies links as links.
        cp -pPR -- "$from/." "$to/" || return 1
    fi
    drop_extras "$to"
    return 0
}

# Remove from the fresh copy $1 what robocopy's /XD and /XF leave out: every
# profile's incremental/ caches, the guard's and the strip's state, the
# snapshot's markers. Never through a link. The copy keeps a link as a link,
# and with a profile that is one (`debug` -> another tree's), the glob
# "$1"/*/incremental names that tree's incremental/, which `rm -rf` would
# empty. `find -P` neither follows a link nor descends into one, and `-type d`
# is a real directory only; `rm -rf` given a link removes the link.
drop_extras() {
    local to=$1
    find -P "$to" -mindepth 2 -maxdepth 3 -type d -name incremental -prune \
        -exec rm -rf -- {} + 2>/dev/null
    rm -rf -- "$to/.susi-guard" "$to/.susi-strip" 2>/dev/null
    rm -f -- "$to/.susi-seed" "$to/.susi-seeded" 2>/dev/null
    return 0
}

# Remove the paths listed one per line in the file $1, a batch per rm.
rm_listed() {
    local list=$1 p failed=0
    local -a batch=()
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        batch+=("$p")
        if [ "${#batch[@]}" -ge 200 ]; then
            rm -rf -- "${batch[@]}" 2>/dev/null || failed=1
            batch=()
        fi
    done <"$list"
    if [ "${#batch[@]}" -gt 0 ]; then
        rm -rf -- "${batch[@]}" 2>/dev/null || failed=1
    fi
    return "$failed"
}

# What the checkout $1's Cargo.lock builds from a registry or a git source
# ($2/keep) and from a path ($2/deny, with the workspace members cargo
# metadata names, which a stale lockfile may lack). One name per line, in
# files: a newline inside an `awk -v` value is refused by some awks. Fails
# when there is no lockfile or it names no package.
unit_lists() {
    local checkout=$1 work=$2 json
    : >"$work/keep" && : >"$work/deny" || return 1
    [ -f "$checkout/Cargo.lock" ] || return 1
    KEEP="$work/keep" DENY="$work/deny" awk '
        function flush() {
            if (inpkg && name != "") {
                n++
                if (src) print name >> (ENVIRON["KEEP"]); else print name >> (ENVIRON["DENY"])
            }
            name = ""; src = 0
        }
        /^\[\[package\]\]/ { flush(); inpkg = 1; next }
        /^\[/ { flush(); inpkg = 0; next }
        inpkg && /^name = "/ { v = $0; sub(/^name = "/, "", v); sub(/".*/, "", v); name = v; next }
        inpkg && /^source = / { src = 1; next }
        END { flush(); exit ((n > 0) ? 0 : 3) }' "$checkout/Cargo.lock" || return 1
    if command -v cargo >/dev/null 2>&1 && [ -f "$checkout/Cargo.toml" ]; then
        json=$(cd "$checkout" && cargo metadata --format-version 1 --no-deps --offline 2>/dev/null) || json=
        # Each package opens with its name, version and id; a dependency
        # entry has no id.
        printf '%s' "$json" | grep -oE '\{"name":"[^"]+","version":"[^"]+","id":' |
            sed 's/^{"name":"\([^"]*\)".*/\1/' >>"$work/deny"
    fi
    return 0
}

# The units among the fingerprint names on stdin ("<package>-<16 hex>") that
# do not come from a registry or git source: a name not kept, or denied.
# $1 is the directory unit_lists filled.
doomed_units() {
    awk '
        phase == 1 { keep[$0] = 1; next }
        phase == 2 { deny[$0] = 1; next }
        {
            n = length($0)
            if (n > 17 && substr($0, n - 16, 1) == "-" && substr($0, n - 15) ~ /^[0-9a-f]+$/) {
                name = substr($0, 1, n - 17)
                if (!(name in keep) || (name in deny)) print
            }
        }' phase=1 "$1/keep" phase=2 "$1/deny" phase=3 -
}

# The hashes of the units among the fingerprint names on stdin that stay
# (their names are kept, and not denied). $1 is the directory unit_lists
# filled.
kept_hashes() {
    awk '
        phase == 1 { keep[$0] = 1; next }
        phase == 2 { deny[$0] = 1; next }
        {
            n = length($0)
            if (n > 17 && substr($0, n - 16, 1) == "-" && substr($0, n - 15) ~ /^[0-9a-f]+$/) {
                name = substr($0, 1, n - 17)
                if ((name in keep) && !(name in deny)) print substr($0, n - 15)
            }
        }' phase=1 "$1/keep" phase=2 "$1/deny" phase=3 -
}

# The entries of the directory $2 (deps/ or build/) that belong to no unit
# whose hash is listed in the file $1: each unit's files there carry its hash
# ("libserde-<hash>.rlib", "serde-<hash>.d", "serde-<hash>/"). What carries no
# hash at all is the workspace's: on Windows cargo writes a binary's
# executable, pdb, import library and dep-info into deps/ without one
# (deps/susi.exe), and a stale one would pass for this branch's.
foreign_entries() {
    DIR="$2" awk '
        phase == 1 { h[$0] = 1; next }
        {
            k = split($0, part, "-"); seg = part[k]; d = index(seg, ".")
            if (d) seg = substr(seg, 1, d - 1)
            if (!(length(seg) == 16 && (seg in h))) print ENVIRON["DIR"] "/" $0
        }' phase=1 "$1" phase=2 -
}

# Strip the build tree $1 down to the units built from a registry or git
# source of the checkout $2's Cargo.lock: every other unit's fingerprint;
# everything in deps/ and build/ but the files of the units that stay (found
# by their hash); and what cargo copies up into a profile for the workspace
# (binaries, libraries, examples), plus incremental/ and tmp/. Prints how many
# units went. Fails, and the tree must not be used, when it cannot tell what
# to keep, cannot list or empty a profile, or anything stripped is still
# there after.
#
# Nothing is deleted through a link: every directory it deletes in (a
# profile, its .fingerprint, deps/ and build/) is checked, each directory
# from the tree down to it (through_link), and a tree with a link where cargo
# keeps builds (holds_foreign_link: a target triple's directory, a profile, a
# triple's profile) is refused before anything goes. Cargo makes no such
# link, and a copy kept one only when made with `cp -a` (--refresh's on Linux
# and macOS) or from a snapshot changed by hand.
strip_units() {
    local target=$1 checkout=$2 work fp profile f sub n=0 rc=0
    if holds_foreign_link "$target"; then
        echo 0
        return 1
    fi
    work=$target/.susi-strip
    discard "$work"
    mkdir "$work" 2>/dev/null || return 1
    if ! unit_lists "$checkout" "$work"; then
        discard "$work"
        return 1
    fi
    for fp in "$target"/*/.fingerprint "$target"/*/*/.fingerprint; do
        [ -e "$fp" ] || [ -L "$fp" ] || continue
        profile=${fp%/.fingerprint}
        if through_link "$target" "$fp" || [ ! -d "$fp" ]; then
            rc=1
            break
        fi
        if ! ls -1 "$fp" >"$work/listed" ||
            ! doomed_units "$work" <"$work/listed" >"$work/doomed" ||
            ! kept_hashes "$work" <"$work/listed" >"$work/kept"; then
            rc=1
            break
        fi
        n=$((n + $(grep -c . "$work/doomed")))
        FP="$fp" awk '{ print ENVIRON["FP"] "/" $0 }' "$work/doomed" >"$work/remove" || {
            rc=1
            break
        }
        for sub in deps build; do
            [ -d "$profile/$sub" ] || continue
            if through_link "$target" "$profile/$sub" || ! ls -1 "$profile/$sub" >"$work/$sub" ||
                ! foreign_entries "$work/kept" "$profile/$sub" <"$work/$sub" >>"$work/remove"; then
                rc=1
                break
            fi
        done
        [ "$rc" -eq 0 ] || break
        # Only workspace targets are copied up into the profile.
        printf '%s\n' "$profile/examples" "$profile/incremental" >>"$work/remove"
        for f in "$profile"/*; do
            [ -f "$f" ] && ! is_link "$f" && printf '%s\n' "$f"
        done >>"$work/remove"
        rm_listed "$work/remove" || rc=1
        [ "$rc" -eq 0 ] || break
        # What is left must be registry and git units only.
        if ! ls -1 "$fp" >"$work/listed" || ! doomed_units "$work" <"$work/listed" >"$work/doomed" ||
            [ -s "$work/doomed" ]; then
            rc=1
            break
        fi
        for sub in deps build; do
            [ -d "$profile/$sub" ] || continue
            if ! ls -1 "$profile/$sub" >"$work/$sub" ||
                ! foreign_entries "$work/kept" "$profile/$sub" <"$work/$sub" >"$work/left" ||
                [ -s "$work/left" ]; then
                rc=1
                break
            fi
        done
        [ "$rc" -eq 0 ] || break
    done
    [ "$rc" -ne 0 ] || [ ! -d "$target/tmp" ] || is_link "$target/tmp" || rm -rf -- "$target/tmp" 2>/dev/null || rc=1
    discard "$work"
    echo "$n"
    return "$rc"
}

# --- a worktree's own target --------------------------------------------------
seed_worktree() {
    local dir=$1 background=$2 top gitdir common primary t seed before after staging
    local lock rc waited dropped copied elsewhere
    # Native git takes a Windows path, also when MSYS_NO_PATHCONV is set.
    ! on_windows || dir=$(cygpath -m "$dir" 2>/dev/null || printf '%s' "$dir")
    top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || return 0
    dir=$top
    gitdir=$(git -C "$dir" rev-parse --path-format=absolute --git-dir 2>/dev/null) || return 0
    common=$(git -C "$dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 0
    # The primary checkout keeps the target it has: it is nobody's worktree,
    # and older worktrees may still link to it (said, when they do).
    if [ "$gitdir" = "$common" ]; then
        note_shared_primary "$dir"
        return 0
    fi
    primary=$(dirname "$common")
    t=$dir/target

    if is_link "$t"; then
        if remove_link "$t"; then
            say "replaced the link to a shared target with this worktree's own ($t)"
        else
            say "could not remove the link at $t; it still points at a shared target"
            return 0
        fi
    fi
    if [ -e "$t" ] && [ ! -d "$t" ]; then
        return 0 # a file is in the way: not ours to move
    fi
    if [ -d "$t" ] && ! holds_nothing "$t"; then
        return 0 # this worktree's own build
    fi
    if no_seed; then
        empty_target "$t" "$dir" "$gitdir"
        return 0
    fi
    # Cargo here builds elsewhere: a copy would be read by no cargo this
    # caller runs, and none is waited for. The link above is still replaced.
    if elsewhere=$(builds_elsewhere "$t"); then
        empty_target "$t" "$dir" "$gitdir"
        say "cargo here builds into $elsewhere ($([ -n "${CARGO_TARGET_DIR:-}" ] && echo CARGO_TARGET_DIR || echo CARGO_BUILD_TARGET_DIR)), not $t: nothing is copied into $t"
        return 0
    fi
    seed=$(seed_dir "$primary")
    if ! usable_seed "$seed" "$primary"; then
        empty_target "$t" "$dir" "$gitdir"
        if [ -e "$seed" ] || [ -L "$seed" ]; then
            say "$seed is no snapshot to copy (not one --refresh published, a checkout, or one whose profile is a link): this worktree's first build compiles everything"
        else
            say "no snapshot at $seed: this worktree's first build compiles everything (scripts/seed-target.sh --refresh makes one)"
        fi
        return 0
    fi
    if [ ! -f "$dir/Cargo.lock" ]; then
        empty_target "$t" "$dir" "$gitdir"
        say "no Cargo.lock in $dir: nothing in the snapshot can be told safe to reuse"
        return 0
    fi
    if [ "$background" = 1 ]; then
        # A real, empty target/ now: what the copy is installed over, and what
        # keeps a tool that links a missing target/ (v0.21.23's `workflow
        # start`, which runs right after the post-checkout hook that calls
        # this) from linking it. The copy runs under this very bash ($BASH),
        # holding none of the caller's descriptors (a lock on fd 9, a pipe a
        # caller reads to its end), and adds to its log. It runs from /, not
        # from the worktree: on Windows a process's current directory cannot
        # be removed, and `git worktree remove` must be able to remove the
        # worktree while its copy runs (the copy then leaves nothing there).
        # CARGO_TARGET_DIR, if set, names $t (builds_elsewhere): the copy
        # needs neither it nor CARGO_BUILD_TARGET_DIR, and a relative one
        # would read as another directory from /.
        empty_target "$t" "$dir" "$gitdir"
        say "copying $seed into $t in the background (log: $gitdir/susi-seed-target.log)"
        (
            unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
            cd / 2>/dev/null
            exec nohup "$BASH" "$self" "$dir"
        ) >>"$gitdir/susi-seed-target.log" 2>&1 </dev/null 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&- &
        return 0
    fi

    # One copy into a worktree at a time. A foreground caller is about to run
    # cargo, and a cold build costs far more than the copy: it waits for one
    # that is running.
    lock=$gitdir/susi-seed-target.lockdir
    waited=0
    while :; do
        take_lock "$lock" "${SUSI_SEED_LOCK_STALE:-7200}"
        rc=$?
        [ "$rc" -eq 1 ] || break
        [ "$waited" -gt 0 ] || say "waiting for the copy into $t that is already running"
        if [ "$waited" -ge "${SUSI_SEED_WAIT_SECS:-1800}" ]; then
            say "gave up waiting after $waited s; this build goes ahead as it is"
            return 0
        fi
        sleep 2
        waited=$((waited + 2))
    done
    if [ "$rc" -ne 0 ]; then
        if worktree_alive "$dir" "$gitdir"; then
            say "cannot make the lock $lock: no copy, so this worktree's first build compiles everything"
        else
            leave_no_trace "$dir" "$t"
            say "$dir was removed: nothing to seed"
        fi
        return 0
    fi
    held_lock=$lock
    # Seeded, or built, while this waited.
    if is_link "$t" || { [ -e "$t" ] && { [ ! -d "$t" ] || ! holds_nothing "$t"; }; }; then
        return 0
    fi
    sweep_staging "${SUSI_SEED_LOCK_STALE:-7200}" "$dir"/target.seed-*.tmp
    staging=$dir/target.seed-$$-$my_since.tmp
    if ! mkdir "$staging" 2>/dev/null || ! claim_staging "$staging"; then
        discard "$staging"
        say "cannot make $staging: no copy, so this worktree's first build compiles everything"
        return 0
    fi

    before=
    [ ! -f "$seed/.susi-seed" ] || read -r before _ 2>/dev/null <"$seed/.susi-seed"
    say "copying $seed${before:+ (origin/main ${before:0:12})} into $t ..."
    copied=0
    copy_tree "$seed" "$staging" && claim_staging "$staging" && copied=1
    # Removed while it was copied (robocopy makes its destination's parents
    # again): nothing of it, or of the copy, is left.
    if ! worktree_alive "$dir" "$gitdir"; then
        discard "$staging"
        leave_no_trace "$dir" "$t"
        say "$dir was removed while the snapshot was copied into it: nothing is left of the copy"
        return 0
    fi
    if [ "$copied" = 0 ]; then
        discard "$staging"
        empty_target "$t" "$dir" "$gitdir"
        say "the copy failed: an empty target instead, and the first build compiles everything"
        return 0
    fi
    after=
    [ ! -f "$seed/.susi-seed" ] || read -r after _ 2>/dev/null <"$seed/.susi-seed"
    if [ "$after" != "$before" ] || ! [ -d "$seed" ]; then
        discard "$staging"
        empty_target "$t" "$dir" "$gitdir"
        say "the snapshot was replaced while it was copied: an empty target instead"
        return 0
    fi
    if ! dropped=$(strip_units "$staging" "$dir"); then
        discard "$staging"
        empty_target "$t" "$dir" "$gitdir"
        say "could not strip the units this worktree builds from its own sources out of the copy: an empty target instead (cold, and correct)"
        return 0
    fi
    if ! printf '%s %s\n' "${before:--}" "$(date +%s)" >"$staging/.susi-seeded" || ! still_mine "$staging"; then
        discard "$staging"
        empty_target "$t" "$dir" "$gitdir"
        say "the copy at $staging changed hands: an empty target instead"
        return 0
    fi
    rm -f -- "$staging/.susi-seed-run"
    if ! worktree_alive "$dir" "$gitdir"; then
        discard "$staging"
        leave_no_trace "$dir" "$t"
        say "$dir was removed while the snapshot was copied into it: nothing is left of the copy"
        return 0
    fi
    if [ -d "$t" ] && ! is_link "$t" && holds_nothing "$t"; then
        rm -f -- "$t/CACHEDIR.TAG" "$t/.rustc_info.json"
        rmdir "$t" 2>/dev/null
    fi
    if [ -e "$t" ] || [ -L "$t" ] || ! rename_dir "$staging" "$t"; then
        discard "$staging"
        say "a build started in $t while the snapshot was copied; it keeps its own"
        return 0
    fi
    # `git worktree remove` that ran between the check above and the rename:
    # its delete may have listed the worktree's entries before the rename
    # added target/, and then leaves the directory behind, holding a full
    # target. What this run just renamed into place is taken back (a real
    # directory it made; discard never reaches through a link).
    if ! worktree_alive "$dir" "$gitdir"; then
        discard "$t"
        leave_no_trace "$dir" "$t"
        say "$dir was removed while the snapshot was copied into it: nothing is left of the copy"
        return 0
    fi
    say "seeded $t; stripped $dropped unit(s) this worktree builds from its own sources"
    return 0
}

# --- the snapshot's one writer ------------------------------------------------
# Same directory, by what it resolves to.
same_dir() {
    local a b
    a=$(canon_dir "$1") || return 1
    b=$(canon_dir "$2") || return 1
    [ "$a" = "$b" ]
}

# The git common dir of the checkout at, or the nearest existing directory
# above, $1.
common_of() {
    local at=$1
    while [ ! -d "$at" ]; do
        [ "$(dirname "$at")" != "$at" ] || return 1
        at=$(dirname "$at")
    done
    git -C "$at" rev-parse --path-format=absolute --git-common-dir 2>/dev/null
}

# Whether --refresh may check out, clean and build in $1: a clone this script
# made (it carries .git/susi-seed-src), or nothing yet; never the primary
# checkout ($3) or anything inside a checkout of this clone ($2), whose branch
# and files belong to whoever works there.
src_allowed() {
    local src=$1 common=$2 primary=$3 sc
    if same_dir "$src" "$primary"; then
        echo "seed: refusing to build in $src: it is the primary checkout (SUSI_TARGET_SEED_SRC must name a checkout of its own)"
        return 1
    fi
    sc=$(common_of "$src") || sc=
    if [ -n "$sc" ] && same_dir "$sc" "$common"; then
        echo "seed: refusing to build in $src: it is inside a checkout of this clone (SUSI_TARGET_SEED_SRC must name a checkout of its own)"
        return 1
    fi
    if { [ -e "$src" ] || [ -L "$src" ]; } && [ ! -f "$src/.git/susi-seed-src" ]; then
        echo "seed: refusing to build in $src: this script did not make it (no .git/susi-seed-src); remove it, or point SUSI_TARGET_SEED_SRC elsewhere"
        return 1
    fi
    return 0
}

# What every unit's hash in the build checkout $1 depends on: the lockfile,
# the toolchain file and the compiler.
src_inputs() {
    {
        cat "$1/Cargo.lock" "$1/rust-toolchain" "$1/rust-toolchain.toml" 2>/dev/null
        (cd "$1" && rustc -vV 2>/dev/null)
    } | git hash-object --stdin 2>/dev/null
}

refresh() {
    local force=$1 log=$2 common primary seed src want have since now got old staging stamp
    local state inputs rec_inputs rec_fresh rec_last size fresh nice tmp stripped mark stale
    no_seed && return 0
    common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 0
    primary=$(dirname "$common")
    seed=$(seed_dir "$primary")
    src=${SUSI_TARGET_SEED_SRC:-$seed-src}
    src=${src//\\//}
    # Only a holder whose liveness cannot be told ages out (holder_alive), so
    # this is far above any build: a day.
    stale=${SUSI_SEED_LOCK_STALE:-86400}
    # The lock's parent: beside the primary checkout by default, but
    # SUSI_TARGET_SEED may name a directory not made yet.
    mkdir -p -- "$(dirname "$seed")" 2>/dev/null
    take_lock "$seed.lock" "$stale"
    case $? in
    0) held_lock=$seed.lock ;;
    1)
        echo "seed: a refresh is already running ($seed.lock)"
        return 0
        ;;
    *)
        echo "seed: cannot make the lock $seed.lock; not refreshing"
        return 0
        ;;
    esac

    want=$(git --git-dir="$common" rev-parse -q --verify 'refs/remotes/origin/main^{commit}' 2>/dev/null) || {
        echo "seed: no origin/main to build"
        return 0
    }
    have= since=0
    [ ! -f "$seed/.susi-seed" ] || read -r have since 2>/dev/null <"$seed/.susi-seed"
    case "$since" in '' | *[!0-9]*) since=0 ;; esac
    if [ "$have" = "$want" ]; then
        echo "seed: already origin/main ($want)"
        return 0
    fi
    now=$(date +%s)
    if [ "$force" = 0 ] && [ -n "$have" ] && [ $((now - since)) -lt "${SUSI_SEED_REFRESH_SECS:-3600}" ]; then
        echo "seed: $((now - since)) s old; refreshed after ${SUSI_SEED_REFRESH_SECS:-3600} s (--force refreshes now)"
        return 0
    fi
    # From here on a refresh runs, or says why it cannot: only then is the log
    # written, so the watcher's tick every few minutes, which mostly finds the
    # snapshot current, never empties the record of the last build.
    if [ -n "$log" ]; then
        exec >"$log" 2>&1
    fi
    echo "seed: $(date '+%F %T') refreshing $seed (${have:-nothing} -> origin/main $want)"
    # Only a snapshot this script made is ever replaced.
    if is_link "$seed" || { [ -e "$seed" ] && [ ! -f "$seed/.susi-seed" ]; }; then
        echo "seed: $seed is not a snapshot this script made; leaving it (point SUSI_TARGET_SEED elsewhere)"
        return 0
    fi
    src_allowed "$src" "$common" "$primary" || return 0
    sweep_staging "$stale" "$seed".new.* "$seed".old.*
    nice=()
    ! command -v nice >/dev/null 2>&1 || nice=(nice -n 19)

    # A clean checkout of origin/main of its own, marked as this script's.
    if [ ! -e "$src" ] && [ ! -L "$src" ]; then
        tmp=$src.clone-$$-$my_since.tmp
        if ! git clone -q --no-checkout "$common" "$tmp" ||
            ! printf 'made by scripts/seed-target.sh --refresh, %s: its checkout and target are rebuilt at will\n' \
                "$(date '+%F %T')" >"$tmp/.git/susi-seed-src" ||
            ! rename_dir "$tmp" "$src"; then
            discard "$tmp"
            echo "seed: cannot clone $common into $src"
            return 0
        fi
    fi
    [ -f "$src/.git/susi-seed-src" ] || return 0
    git -C "$src" fetch -q --no-tags "$common" "+refs/remotes/origin/main:refs/remotes/seed/main" || {
        echo "seed: cannot fetch origin/main into $src"
        return 0
    }
    got=$(git -C "$src" rev-parse -q --verify 'refs/remotes/seed/main^{commit}') || return 0
    # Everything but the build: `target`, not `/target`, which Git Bash
    # would hand native git as a Windows path.
    git -C "$src" -c advice.detachedHead=false checkout -q --force --detach "$got" &&
        git -C "$src" clean -q -fdx -e target || {
        echo "seed: cannot check out $got in $src"
        return 0
    }

    # The build checkout's target only grows: cargo never removes a unit whose
    # hash went out of use. It starts over when what every hash depends on
    # changes, or once it is twice its size after its last fresh build.
    state=$src/.git/susi-seed-src.state
    rec_inputs= rec_fresh=0 rec_last=0
    [ ! -f "$state" ] || read -r rec_inputs rec_fresh rec_last 2>/dev/null <"$state"
    case "$rec_fresh" in '' | *[!0-9]*) rec_fresh=0 ;; esac
    case "$rec_last" in '' | *[!0-9]*) rec_last=0 ;; esac
    inputs=$(src_inputs "$src")
    fresh=0
    if [ ! -d "$src/target" ] || is_link "$src/target"; then
        fresh=1
    elif [ "$inputs" != "$rec_inputs" ]; then
        echo "seed: Cargo.lock, the toolchain or rustc changed; starting $src/target over"
        fresh=1
    elif [ "$rec_fresh" -gt 0 ] && [ "$rec_last" -gt $((2 * rec_fresh)) ]; then
        echo "seed: $src/target grew to ${rec_last} KiB, over twice its fresh ${rec_fresh} KiB; starting it over"
        fresh=1
    fi
    [ "$fresh" = 0 ] || discard "$src/target"

    echo "seed: building $got in $src"
    if ! (
        cd "$src" || exit 1
        unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
        ${nice[@]+"${nice[@]}"} cargo build --workspace --all-targets --locked || exit 1
        ${nice[@]+"${nice[@]}"} cargo clippy --workspace --all-targets --locked ||
            echo "seed: clippy did not pass on $got; publishing the build anyway"
    ); then
        echo "seed: the build of $got failed; the snapshot stays at ${have:-nothing}"
        return 0
    fi
    size=$(du -sk "$src/target" 2>/dev/null | cut -f1)
    case "$size" in '' | *[!0-9]*) size=0 ;; esac
    [ "$fresh" = 0 ] && [ "$rec_fresh" -gt 0 ] || rec_fresh=$size
    printf '%s %s %s\n' "${inputs:--}" "$rec_fresh" "$size" >"$state" 2>/dev/null

    # A staging directory of this run's own; another run's is never touched.
    staging=$seed.new.$$-$my_since
    if ! mkdir "$staging" 2>/dev/null || ! claim_staging "$staging"; then
        discard "$staging"
        echo "seed: cannot make $staging"
        return 0
    fi
    echo "seed: copying $src/target to $staging"
    if ! copy_tree "$src/target" "$staging" || ! claim_staging "$staging"; then
        discard "$staging"
        echo "seed: the copy failed"
        return 0
    fi
    if ! stripped=$(strip_units "$staging" "$src"); then
        discard "$staging"
        echo "seed: could not strip the workspace's own units from $staging; the snapshot stays at ${have:-nothing}"
        return 0
    fi
    echo "seed: stripped $stripped unit(s) built from a path; the snapshot keeps registry and git crates only"
    stamp="$got $(date +%s)"
    mark=
    if ! printf '%s\n' "$stamp" >"$staging/.susi-seed" || ! read -r mark 2>/dev/null <"$staging/.susi-seed" ||
        [ "$mark" != "$stamp" ] || ! still_mine "$staging"; then
        discard "$staging"
        echo "seed: $staging did not keep its marker, or is no longer this run's; nothing published"
        return 0
    fi

    # Publish: the old snapshot out of the way, the new one in. A worktree
    # copying the old one holds it open on Windows; then this run's copy is
    # dropped and the next refresh tries again.
    old=
    if [ -d "$seed" ]; then
        old=$seed.old.$$-$my_since
        if ! rename_dir "$seed" "$old"; then
            discard "$staging"
            echo "seed: $seed is in use; the next refresh publishes"
            return 0
        fi
        claim_staging "$old"
    fi
    if ! rename_dir "$staging" "$seed"; then
        [ -z "$old" ] || rename_dir "$old" "$seed"
        discard "$staging"
        echo "seed: could not move $staging into place"
        return 0
    fi
    [ -z "$old" ] || discard "$old"
    echo "seed: published $got at $seed"
    return 0
}

mode=seed
background=0
force=0
log=
dir=
while [ $# -gt 0 ]; do
    case "$1" in
    --refresh) mode=refresh ;;
    --background) background=1 ;;
    --force) force=1 ;;
    --log)
        log=${2:-}
        shift
        ;;
    -h | --help)
        sed -n '2,11p' "${BASH_SOURCE[0]}"
        exit 0
        ;;
    -*)
        echo 'usage: seed-target.sh [--background] [DIR] | --refresh [--force] [--log FILE]' >&2
        exit 2
        ;;
    *) dir=$1 ;;
    esac
    shift
done

if [ "$mode" = refresh ]; then
    refresh "$force" "$log"
else
    [ -n "$dir" ] || dir=$(git rev-parse --show-toplevel 2>/dev/null) || dir=
    [ -n "$dir" ] && [ -d "$dir" ] && seed_worktree "$dir" "$background"
fi
exit 0
