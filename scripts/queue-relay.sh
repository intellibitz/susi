#!/usr/bin/env bash
# Apply one susi queue request where the pusher's transport could not.
#
#   scripts/queue-relay.sh <commit> [--remote <name>] [--now <unix>]
#   scripts/queue-relay.sh --sweep [--remote <name>] [--now <unix>] [--grace <secs>]
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

# Allowed queue namespaces: "refs/claims/", "refs/claim-agents/", "refs/claim-generation/", "refs/closed/", "refs/abandoned/", "refs/release-cuts/"
USAGE='usage: queue-relay.sh <commit> [--remote <name>] [--now <unix>]
       queue-relay.sh --sweep [--remote <name>] [--now <unix>] [--grace <secs>]'

LANDED=0
REFUSED=1
MALFORMED=2
EXPIRED=3

remote="origin"
now=""
grace=""
positional=()

while [ "$#" -gt 0 ]; do
    case "$1" in
    --remote)
        [ "$#" -ge 2 ] || { printf 'queue-relay: --remote needs a value\n%s\n' "$USAGE" >&2; exit "$MALFORMED"; }
        remote=$2
        shift 2
        ;;
    --now)
        [ "$#" -ge 2 ] && [[ "$2" =~ ^-?[0-9]+$ ]] || { printf 'queue-relay: --now needs an integer\n%s\n' "$USAGE" >&2; exit "$MALFORMED"; }
        now=$2
        shift 2
        ;;
    --grace)
        [ "$#" -ge 2 ] && [[ "$2" =~ ^-?[0-9]+$ ]] || { printf 'queue-relay: --grace needs an integer\n%s\n' "$USAGE" >&2; exit "$MALFORMED"; }
        grace=$2
        shift 2
        ;;
    --sweep)
        positional+=("--sweep")
        shift
        ;;
    -*)
        printf '%s\n' "$USAGE" >&2
        exit "$MALFORMED"
        ;;
    *)
        positional+=("$1")
        shift
        ;;
    esac
done

[ -n "$now" ] || now=$(date +%s)

sweep_requests() {
    local sweep_grace=${grace:-60} listed status=0 line ref leaf stamp err
    if ! listed=$(git ls-remote "$remote" "refs/heads/susi-queue/*" 2>&1); then
        echo "queue-relay: cannot list requests on $remote: $listed" >&2
        exit 1
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        ref=$(awk '{print $2}' <<<"$line")
        case "$ref" in
        refs/heads/susi-queue/*) ;;
        *) continue ;;
        esac
        leaf=${ref##*/}
        stamp=${leaf%%.*}
        if [[ ! "$stamp" =~ ^[0-9]+$ ]] || [ $(( stamp + sweep_grace )) -ge "$now" ]; then
            continue
        fi
        if err=$(git push --quiet "$remote" ":$ref" 2>&1); then
            echo "queue-relay: swept ${ref#refs/heads/} (expired at $stamp)"
        else
            status=1
            echo "queue-relay: could not sweep $ref: $err" >&2
        fi
    done <<<"$listed"
    exit "$status"
}

if [ "${#positional[@]}" -eq 1 ] && [ "${positional[0]}" = "--sweep" ]; then
    sweep_requests
fi

if [ "${#positional[@]}" -ne 1 ]; then
    printf '%s\n' "$USAGE" >&2
    exit "$MALFORMED"
fi

commit=${positional[0]}
if ! message=$(git log -1 --format=%B "$commit" 2>&1); then
    echo "queue-relay: cannot read $commit: $message" >&2
    exit "$MALFORMED"
fi

# Split on LF only so U+2028/U+2029 inside JSON strings are never split.
req_line=$(awk '
    {
        line = $0
        sub(/^[[:space:]]+/, "", line)
        sub(/[[:space:]]+$/, "", line)
        if (substr(line, 1, 1) == "{") {
            last = line
        }
    }
    END {
        if (last != "") print last
    }
' <<<"$message")

if [ -z "$req_line" ]; then
    echo "queue-relay: refusing $commit: no request in the commit message" >&2
    exit "$MALFORMED"
fi

if ! jq -e . >/dev/null 2>&1 <<<"$req_line"; then
    echo "queue-relay: refusing $commit: request is not JSON" >&2
    exit "$MALFORMED"
fi

validation_err=$(jq -r '
    def ref_ok:
        type == "string" and (
            test("^refs/(claims|claim-agents|claim-generation|closed|abandoned|release-cuts)/[A-Za-z0-9][A-Za-z0-9._-]*$")
        );
    def sha_ok:
        type == "string" and test("^([0-9a-f]{40}|[0-9a-f]{64})?$");
    if type != "object" or .v != 1 then
        "not a v1 queue request"
    elif (.expires_unix | type) != "number" or (.expires_unix | floor) != .expires_unix or .expires_unix < 0 then
        "expires_unix must be a unix time"
    elif ((.leases // []) | type) != "array" or (.updates | type) != "array" or (.updates | length) == 0 then
        "leases must be a list and updates a non-empty list"
    elif any((.leases // [])[]; (type != "array") or (length != 2) or ((.[0] | ref_ok) | not) or ((.[1] | sha_ok) | not)) then
        "lease is not [<queue ref>, <sha or empty>]"
    elif any(.updates[]; (type != "object") or ((.ref | ref_ok) | not)) then
        "update does not name a queue ref (refs/claims/, refs/claim-agents/, refs/claim-generation/, refs/closed/, refs/abandoned/, refs/release-cuts/)"
    elif any(.updates[];
        if .delete == true and .blob == null then false
        elif .delete == null and (.blob | type) == "string" and (.blob | utf8bytelength) <= 65536 then
            ((.blob | try (fromjson | false) catch true))
        else true
        end
    ) then
        "update must carry a JSON blob of at most 65536 bytes, or delete: true"
    else
        empty
    end
' <<<"$req_line")

if [ -n "$validation_err" ]; then
    echo "queue-relay: refusing $commit: $validation_err" >&2
    exit "$MALFORMED"
fi

expires=$(jq -r '.expires_unix' <<<"$req_line")
touched=$(jq -r '[.updates[].ref] | join(", ")' <<<"$req_line")

if [ "$now" -gt "$expires" ]; then
    echo "queue-relay: $commit expired at $expires ($(( now - expires ))s ago); its client has stopped waiting, so $touched is left as it is" >&2
    exit "$EXPIRED"
fi

push_args=(push --quiet --atomic)
while IFS= read -r lease_arg; do
    [ -n "$lease_arg" ] && push_args+=("$lease_arg")
done < <(jq -r '(.leases // [])[] | "--force-with-lease=\(.[0]):\(.[1])"' <<<"$req_line")
push_args+=("$remote")

num_updates=$(jq -r '.updates | length' <<<"$req_line")
for (( i = 0; i < num_updates; i++ )); do
    ref=$(jq -r ".updates[$i].ref" <<<"$req_line")
    is_del=$(jq -r ".updates[$i].delete // false" <<<"$req_line")
    if [ "$is_del" = "true" ]; then
        push_args+=(":$ref")
    else
        # Extract raw string without trailing newline so blob sha matches client byte-for-byte.
        if ! blob_sha=$(jq -j ".updates[$i].blob" <<<"$req_line" | git hash-object -w --stdin 2>&1); then
            echo "queue-relay: cannot store a blob of $commit: $blob_sha" >&2
            exit "$MALFORMED"
        fi
        push_args+=("${blob_sha}:$ref")
    fi
done

if ! push_err=$(git "${push_args[@]}" 2>&1); then
    echo "queue-relay: $remote refused $touched: $push_err" >&2
    exit "$REFUSED"
fi

echo "queue-relay: relayed $touched to $remote"
exit "$LANDED"
