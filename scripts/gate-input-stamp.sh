#!/usr/bin/env bash
# Hash complete source and tool inputs in one pass. Git owns all index/ref
# interpretation; sha256sum frames the data without Python.
set -euo pipefail

kind=${1:-gate}
case "$kind" in
gate | --format) ;;
*) exit 2 ;;
esac

repo=$(git rev-parse --show-toplevel)
cd "$repo"

run_tool_version() {
    local tool=$1
    if [ -f "$tool" ]; then
        case "$tool" in
        *.cmd | *.bat)
            cmd.exe //c "$tool" --version 2>/dev/null || "$tool" --version
            return
            ;;
        *.exe)
            "$tool" --version
            return
            ;;
        *)
            sh "$tool" --version
            return
            ;;
        esac
    fi
    "$tool" --version
}

{
    printf 'version\0susi-proof-v3\0'
    printf 'kind\0%s\0' "$kind"
    printf 'index\0'
    git ls-files --stage -z -- . ':(exclude).agents/tasks'
    printf '\0edits\0'
    git diff --binary --no-ext-diff --no-textconv -- . ':(exclude).agents/tasks'
    printf '\0untracked\0'
    while IFS= read -r -d '' rel; do
        [ -n "$rel" ] || continue
        printf 'path\0%s\0content\0' "$rel"
        sha256sum "$rel" | awk '{print $1}'
    done < <(git ls-files --others --exclude-standard -z -- . ':(exclude).agents/tasks')
    printf '\0env\0'
    LC_ALL=C env | LC_ALL=C sort | awk '
        /^(CARGO_|RUST)/ {
            printf "%s\0", $0
        }
    '
    printf '\0configs\0'
    cur=$repo
    while :; do
        for name in config config.toml; do
            cfg="$cur/.cargo/$name"
            if [ -f "$cfg" ]; then
                printf 'cfg\0%s\0' "$cfg"
                sha256sum "$cfg" | awk '{print $1}'
            fi
        done
        parent=$(dirname "$cur")
        [ "$parent" != "$cur" ] || break
        cur=$parent
    done
    cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
    for name in config config.toml; do
        cfg="$cargo_home/$name"
        if [ -f "$cfg" ]; then
            printf 'cfg\0%s\0' "$cfg"
            sha256sum "$cfg" | awk '{print $1}'
        fi
    done
    printf '\0formatter\0'
    run_tool_version "${RUSTFMT:-rustfmt}"
    if [ "$kind" != "--format" ]; then
        printf '\0compiler\0'
        run_tool_version "${RUSTC:-rustc}"
        printf '\0gate-policy\0%s\0' "${SUSI_LOCAL_GATE:-}"
        printf 'scope\0'
        git diff --binary --no-ext-diff --no-textconv origin/main -- . ':(exclude).agents/tasks'
    fi
} | sha256sum | awk '{print $1}'
