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

here=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
mine=$(token_of "$here")
[ -n "$mine" ] || exit 0

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
