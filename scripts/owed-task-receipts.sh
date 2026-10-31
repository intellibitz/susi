#!/usr/bin/env bash
# Print task IDs closed by <agent> whose close is not present on origin/main.
# Read all close receipt blobs in one Git process and parse them in one jq run.
set -uo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

agent=${1:?usage: owed-task-receipts.sh <agent>}
remote=${2:-origin}

# A single Git batch and tree listing avoids starting one Git process per
# receipt. If Python or the batch is unavailable, retain the shell path.
if command -v python3 >/dev/null 2>&1 && python3 - "$agent" "$remote" <<'PY'
import io
import json
import subprocess
import sys

agent, remote = sys.argv[1:]

def git(*args, data=None, check=True):
    return subprocess.run(
        ['git', *args], input=data, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, check=check,
    )

try:
    refs = git('for-each-ref', '--format=%(refname:strip=2) %(objectname)',
               'refs/closed/').stdout.splitlines()
    pairs = [line.split(b' ', 1) for line in refs]
    if any(len(pair) != 2 for pair in pairs):
        raise ValueError('malformed ref list')
    batch = git('cat-file', '--batch',
                data=b''.join(oid + b'\n' for _, oid in pairs)).stdout
    stream = io.BytesIO(batch)
    owned = []
    for name, _ in pairs:
        header = stream.readline().split()
        if len(header) != 3 or header[1] != b'blob':
            raise ValueError('invalid receipt object')
        size = int(header[2])
        body = stream.read(size)
        if len(body) != size or stream.read(1) != b'\n':
            raise ValueError('incomplete receipt object')
        try:
            receipt = json.loads(body)
            if isinstance(receipt, dict) and receipt.get('agent') == agent:
                owned.append(name)
        except (ValueError, UnicodeError):
            pass
    published = set()
    if owned:
        main = git('rev-parse', '--verify', '--quiet',
                   f'refs/remotes/{remote}/main^{{commit}}', check=False)
        if main.returncode == 0:
            tree = git('ls-tree', '-r', '-t', '-z', '--name-only',
                       main.stdout.strip().decode('ascii'), '--',
                       '.agents/tasks/done/', check=False)
            if tree.returncode == 0:
                published = set(tree.stdout.split(b'\0'))
    output = [name for name in owned
              if b'.agents/tasks/done/' + name + b'.json' not in published]
    sys.stdout.buffer.write(b''.join(name + b'\n' for name in output))
except (OSError, subprocess.SubprocessError, ValueError, UnicodeError):
    sys.exit(1)
PY
then
    exit 0
fi

main_commit=$(git rev-parse --verify --quiet "refs/remotes/$remote/main^{commit}" 2>/dev/null || true)
refs=$(git for-each-ref --format='%(refname:strip=2) %(objectname)' refs/closed/ 2>/dev/null || true)
[ -n "$refs" ] || exit 0

ids=$(awk '{print $1}' <<<"$refs")
objects=$(awk '{print $2}' <<<"$refs")
# Receipts are single-line JSON blobs, with or without a final newline.
# Discard batch headers and separators rather than assuming a fixed line count.
bodies=$(printf '%s\n' "$objects" | git cat-file --batch 2>/dev/null | awk 'NF && !/^[0-9a-f]+ blob [0-9]+$/ { print }')
pairs=$(paste <(printf '%s\n' "$ids") <(printf '%s\n' "$bodies"))
jq -Rrn --arg agent "$agent" \
    '[inputs | split("\t") as $p | ($p[1] | fromjson? // {}) as $receipt | select($receipt.agent == $agent) | $p[0]] | .[]' \
    <<<"$pairs" 2>/dev/null | while IFS= read -r id; do
        [ -n "$id" ] || continue
        if [ -n "$main_commit" ] && git cat-file -e "$main_commit:.agents/tasks/done/$id.json" 2>/dev/null; then
            : # Published close; not outstanding merge debt.
        else
            printf '%s\n' "$id"
        fi
    done
