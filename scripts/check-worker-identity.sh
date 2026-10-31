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

# Return small values through globals: command substitution would fork a shell
# per field per worktree, even after removing the external tools.
gitdir_of() {
    local path=$1 line gitdir
    resolved_gitdir=
    if [ -d "$path/.git" ]; then
        resolved_gitdir=$path/.git
    elif [ -f "$path/.git" ] && IFS= read -r line <"$path/.git" && [[ "$line" = 'gitdir: '* ]]; then
        gitdir=${line#gitdir: }
        gitdir=${gitdir%$'\r'}
        if [[ "$gitdir" = /* || "$gitdir" =~ ^[A-Za-z]: || "$gitdir" = \\\\* ]]; then
            resolved_gitdir=$gitdir
        else
            resolved_gitdir=$path/$gitdir
        fi
    else
        resolved_gitdir=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null)
    fi
}

# Match the existing local-value rule without starting sed and tail for every
# worktree. Git still resolves inherited configuration when no local token exists.
worktree_value() {
    local file=$1 key=$2 line
    worktree_result=
    [ -f "$file" ] || return 0
    while IFS= read -r line || [ -n "$line" ]; do
        if [[ "$line" =~ ^[[:space:]]*$key[[:space:]]*=[[:space:]]*(.*)$ ]]; then
            worktree_result=${BASH_REMATCH[1]}
        fi
    done <"$file"
}

canonicalize() {
    local LC_ALL=C
    canonical=${1^^}
    canonical=${canonical//[^A-Z0-9]/}
}

token_of() {  # a worktree's own token, never a borrowed one
    local path=$1 LC_ALL=C
    worker_token=
    worker_token_present=0
    gitdir_of "$path" || return 0
    worktree_value "$resolved_gitdir/config.worktree" agent
    worker_token=$worktree_result
    if [[ ! "$worker_token" =~ ^[A-Za-z0-9_-]+$ ]]; then
        worker_token=$(git -C "$path" config --get susi.agent 2>/dev/null)
    fi
    [ -z "$worker_token" ] || worker_token_present=1
    canonicalize "$worker_token"
    worker_token=$canonical
    return 0
}

author_of() {  # the git author this worktree's own config sets, never the clone-wide one
    worker_author=
    gitdir_of "$1" || return 0
    worktree_value "$resolved_gitdir/config.worktree" name
    worker_author=$worktree_result
    if [ -n "$worker_author" ] && [[ ! "$worker_author" =~ ^[A-Za-z0-9_-]+$ ]]; then
        worker_author=$(git -C "$1" config --worktree --get user.name 2>/dev/null)
    fi
}

here=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
token_of "$here"
mine=$worker_token
here_gitdir=$resolved_gitdir
if [ -z "$mine" ] && [ "$worker_token_present" = 1 ]; then
    echo '❌ invalid worker token — run: susi workflow own-identity' >&2
    exit 1
fi
[ -n "$mine" ] || exit 0

# Whose worktree this is comes before who else shares it. The git login
# (INTELLIBITZ) is one name for every agent on the machine, and PRIMARY, MAIN…
# name a place: a worker that answers to either, or whose commits are authored
# by the login, has no identity of its own, and every task id and claim minted
# under it is the next agent's collision. The primary checkout is nobody's —
# except a cloud session's clone, which is its worker's own and only checkout
# (scripts/cloud-session.sh) and is judged like any worktree.
cloud_session() { [ -x "$here/scripts/cloud-session.sh" ] && "$here/scripts/cloud-session.sh"; }
if [ ! "$here_gitdir" -ef "$(git rev-parse --path-format=absolute --git-common-dir)" ] || cloud_session; then
    logins=' '
    while IFS= read -r -d '' scope && IFS= read -r -d '' login; do
        case "$scope" in
        system|global|local)
            canonicalize "$login"
            [ -z "$canonical" ] || logins+="$canonical " ;;
        esac
    done < <(git config --show-scope --null --get-all user.name 2>/dev/null || true)
    me=$mine
    reserved=""
    case " PRIMARY MAIN MASTER AGENT USER SUSI " in *" $me "*) reserved=1 ;; esac
    case "$logins" in *" $me "*) reserved=1 ;; esac
    author_of "$here"
    canonicalize "$worker_author"
    author=$canonical
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
while IFS= read -r -d '' row; do
    case "$row" in 'worktree '*) path=${row#worktree } ;; *) continue ;; esac
    [ "$path/.git" -ef "$here/.git" ] && continue
    token_of "$path"
    [ "$worker_token" = "$mine" ] && others="$others $path"
done < <(git worktree list --porcelain -z)
if ! wait "$!"; then
    echo '❌ cannot verify the worktree identity index' >&2
    exit 1
fi

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
