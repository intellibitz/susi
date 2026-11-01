#!/usr/bin/env bash
set -euo pipefail
scripts=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$scripts/../.github/workflows/test.yml"
# Release tags and forced proofs must never use the main-only reuse path.
grep -Fq "github.ref == 'refs/heads/main' && steps.stamp.outputs.proven == 'true' && inputs.prove != true" "$workflow"
grep -Fq "needs.codetree.outputs.run_windows == 'true' && needs.codetree.outputs.windows_proven != 'true'" "$workflow"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/repo/scripts" "$tmp/bin"
cp "$scripts/ci-windows-proven.sh" "$tmp/repo/scripts/"
cat > "$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
case "$1 $2" in
  'run list')
    [ "${MODE:-}" != list_error ] || exit 1
    [[ " $* " == *' --created >='* ]] || exit 1
    printf '42\t%s\n' "$CANDIDATE_SHA" ;;
  'run view')
    [ "${MODE:-}" != view_error ] || exit 1
    conclusion=success
    name='Windows binary (release e2e checks)'
    [ "${MODE:-}" != skipped ] || conclusion=skipped
    [ "${MODE:-}" != wrong_job ] || name='Unrelated Windows job'
    jq -n --arg name "$name" --arg conclusion "$conclusion" \
      '{jobs:[{name:$name,conclusion:$conclusion}]}' | jq -r "${@: -1}" ;;
  *) exit 1 ;;
esac
MOCK
chmod +x "$tmp/bin/gh" "$tmp/repo/scripts/ci-windows-proven.sh"
export PATH="$tmp/bin:$PATH"
cd "$tmp/repo"
git init -q
git config user.name 'Test Worker'
git config user.email 'test@example.invalid'
printf 'source\n' > source.txt
git add source.txt
git commit -qm initial
export CANDIDATE_SHA
CANDIDATE_SHA=$(git rev-parse HEAD)
check=scripts/ci-windows-proven.sh
"$check" HEAD
for MODE in skipped wrong_job view_error list_error; do
    export MODE
    if "$check" HEAD; then
        echo "unexpected reuse with $MODE" >&2
        exit 1
    fi
done
unset MODE
mkdir -p .agents/tasks
printf 'task\n' > .agents/tasks/task.json
git add .agents/tasks/task.json
git commit -qm task
"$check" HEAD
printf 'changed\n' > source.txt
git add source.txt
git commit -qm source
if "$check" HEAD; then
    echo 'unexpected reuse after source changed' >&2
    exit 1
fi
printf 'Windows proof reuse checks passed\n'
