#!/usr/bin/env bash
# One token per worker.
#
# A worktree created outside scripts/susi-worktree.sh inherits the creating
# tree's susi.agent, so two worktrees answer to one identity and can renew, close
# and release each other's claims. That is live here: the primary checkout and a
# hand-made worktree under .claude/worktrees both carry PRIMARY.
#
# Each worktree's token is read from that worktree's own config.worktree. Asking
# git for a --worktree value from outside the worktree does not return its value -
# an earlier attempt did that and reported every worktree as sharing one identity.
#
#   scripts/check-worker-identity.sh
# git exports GIT_DIR (and friends) to hooks, which makes `git -C <other
# worktree>` read *this* repository's config - the reason an earlier version
# reported every worktree as sharing one identity. Neutralise them first.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

set -uo pipefail

token_of() {  # a worktree's own token, never a borrowed one
    local path=$1 gitdir token
    gitdir=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null) || return 0
    token=$(sed -n 's/^[[:space:]]*agent[[:space:]]*=[[:space:]]*//p' "$gitdir/config.worktree" 2>/dev/null | tail -1)
    if [ -z "$token" ]; then
        token=$(git -C "$path" config --get susi.agent 2>/dev/null)
    fi
    printf '%s' "$token"
}

author_of() {  # the git author this worktree's own config sets, never the clone-wide one
    local gitdir
    gitdir=$(git -C "$1" rev-parse --absolute-git-dir 2>/dev/null) || return 0
    sed -n 's/^[[:space:]]*name[[:space:]]*=[[:space:]]*//p' "$gitdir/config.worktree" 2>/dev/null | tail -1
}

norm() { tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9\n'; }

here=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
mine=$(token_of "$here")
[ -n "$mine" ] || exit 0

# Whose worktree this is comes before who else shares it. The git login
# (INTELLIBITZ) is one name for every agent on the machine, and PRIMARY, MAIN…
# name a place: a worker that answers to either, or whose commits are authored
# by the login, has no identity of its own, and every task id and claim minted
# under it is the next agent's collision. The primary checkout is nobody's.
if [ "$(realpath "$(git rev-parse --absolute-git-dir)")" != "$(realpath "$(git rev-parse --path-format=absolute --git-common-dir)")" ]; then
    logins=$( { for scope in --system --global --local; do git config "$scope" --get-all user.name 2>/dev/null; done; } | norm)
    me=$(printf '%s\n' "$mine" | norm)  # compared as the tokens susi mints: upper-case alphanumerics
    reserved=""
    case " PRIMARY MAIN MASTER AGENT USER SUSI " in *" $me "*) reserved=1 ;; esac
    printf '%s\n' "$logins" | grep -qx "$me" && reserved=1
    author=$(author_of "$here" | norm)
    if [ -n "$reserved" ] || [ "$author" != "$me" ]; then
        if [ -n "$reserved" ]; then
            printf '❌ this worktree answers to %s, which is the shared git login or a role, not a worker\n' "$mine" >&2
        else
            printf '❌ commits here would be authored by the shared git login, not by %s\n' "$mine" >&2
        fi
        cat >&2 <<'EOF'
   Every agent on this machine shares that name, so task ids and claims minted
   under it collide with the next agent's. Give this worktree its own:
     susi workflow own-identity
   (an installed susi older than that command: pick <TOOL><WORKTREE-ID>, e.g.
   CLAUDESWEETKHORANA5D3FC4, and set it for the token and the author)
     git config extensions.worktreeConfig true
     git config --worktree susi.agent <UNIQUE-NAME>
     git config --worktree user.name  <UNIQUE-NAME>
     git config --worktree user.email <unique-name>@localhost
   A claim travels with its token: `susi tasks release <task>` first if you hold one.
EOF
        exit 1
    fi
fi

others=""
while IFS= read -r path; do
    [ -n "$path" ] || continue
    [ "$(realpath "$path")" = "$(realpath "$here")" ] && continue
    [ "$(token_of "$path")" = "$mine" ] && others="$others $path"
done < <(git worktree list --porcelain | awk '/^worktree /{print $2}')

if [ -n "$others" ]; then
    printf '❌ one token per worker: this worktree and%s both answer to %s\n' "$others" "$mine" >&2
    cat >&2 <<'EOF'
   Two worktrees with one identity can renew, close and release each other's
   claims. Give this one its own token, in this order:
     susi tasks release <your task>        # a claim travels with its token
     git config --worktree susi.agent <UNIQUE-NAME>
     susi tasks claim <your task> --scope <paths>
EOF
    exit 1
fi
echo "✅ ${mine} is this worktree's token alone"
exit 0
