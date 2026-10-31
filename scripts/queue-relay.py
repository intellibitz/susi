#!/usr/bin/env python3
"""Apply one susi queue request where the pusher's transport could not.

A cloud session's git proxy pushes branches only. So when `susi tasks`
(crates/susi-gawd/src/admin/queue_relay.rs) has a compare-and-swap push of
the queue's refs refused while every lease still holds, it relays the
transaction as a commit on refs/heads/susi-queue/<agent>/<expires>.<nonce>:
HEAD's tree, so this file and .github/workflows/queue.yml travel with it, and
the transaction in the commit message. That workflow runs this for the pushed
commit with the job's token, which may write any ref the rulesets leave open,
deletes the request, and runs `--sweep`. The leases are the pusher's, so the
replay lands exactly what the pusher proved, or nothing.

    scripts/queue-relay.py <commit> [--remote <name>] [--now <unix>]
    scripts/queue-relay.py --sweep [--remote <name>] [--now <unix>] [--grace <secs>]

Only the queue's own namespaces may be written (refs/claims/,
refs/claim-agents/, refs/claim-generation/, refs/closed/, refs/abandoned/,
and a release cut's refs/release-cuts/): a
branch, a tag or the merger's attestations (refs/merged/, refs/verified/)
never travel this way. A request past its expiry is never applied: the client
stops waiting then and reports that it will not be, so applying it later would
contradict what the agent was told.

Exits 0 when the push landed, 1 when the remote refused it (a lease that no
longer held: the lost race the pusher reads back), 2 for a commit that is not
a request this relay applies, and 3 for a request past its expiry.

`--sweep` deletes every request branch whose expiry (the leading number of its
last path component) is more than `--grace` seconds (60) in the past — the
debris of a run that never came. A cloud session cannot delete a branch, so
the relay is the one place that can.
"""
import json
import re
import subprocess
import sys
import time

NAMESPACES = (
    "refs/claims/",
    "refs/claim-agents/",
    "refs/claim-generation/",
    "refs/closed/",
    "refs/abandoned/",
    "refs/release-cuts/",
)
REQUESTS = "refs/heads/susi-queue/"
NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
SHA = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})?$")
# The client's limit, crates/susi-gawd/src/admin/queue_relay.rs MAX_BLOB_BYTES.
MAX_BLOB = 65536
USAGE = (
    "usage: queue-relay.py <commit> [--remote <name>] [--now <unix>]\n"
    "       queue-relay.py --sweep [--remote <name>] [--now <unix>] [--grace <secs>]"
)

LANDED, REFUSED, MALFORMED, EXPIRED = 0, 1, 2, 3


class Malformed(Exception):
    """The commit does not carry a request this relay applies."""


def git(*args, stdin=None):
    return subprocess.run(
        ["git", *args], input=stdin, capture_output=True, check=True
    ).stdout


def ref_ok(ref):
    return isinstance(ref, str) and any(
        ref.startswith(ns) and NAME.match(ref[len(ns):]) for ns in NAMESPACES
    )


def parse(message):
    """The request is the last line of the message that is a JSON object.

    Lines are split on "\\n" only: the client's JSON escapes every control
    character, but leaves U+2028 and friends raw, and str.splitlines() would
    cut a request carrying one in two.
    """
    for line in reversed(message.split("\n")):
        line = line.strip()
        if line.startswith("{"):
            try:
                return json.loads(line)
            except ValueError as exc:
                raise Malformed(f"request is not JSON: {exc}") from exc
    raise Malformed("no request in the commit message")


def validate(doc):
    if not isinstance(doc, dict) or doc.get("v") != 1:
        raise Malformed("not a v1 queue request")
    expires = doc.get("expires_unix")
    if not isinstance(expires, int) or isinstance(expires, bool) or expires < 0:
        raise Malformed("expires_unix must be a unix time")
    leases = doc.get("leases", [])
    updates = doc.get("updates")
    if not isinstance(leases, list) or not isinstance(updates, list) or not updates:
        raise Malformed("leases must be a list and updates a non-empty list")
    for lease in leases:
        if not (
            isinstance(lease, list)
            and len(lease) == 2
            and ref_ok(lease[0])
            and isinstance(lease[1], str)
            and SHA.match(lease[1])
        ):
            raise Malformed(f"lease {lease!r} is not [<queue ref>, <sha or empty>]")
    for update in updates:
        if not isinstance(update, dict) or not ref_ok(update.get("ref")):
            raise Malformed(
                f"update {update!r} does not name a queue ref ({', '.join(NAMESPACES)})"
            )
        blob, delete = update.get("blob"), update.get("delete")
        if delete is True and blob is None:
            continue
        if delete is None and isinstance(blob, str) and len(blob.encode()) <= MAX_BLOB:
            try:
                json.loads(blob)
            except ValueError as exc:
                raise Malformed(
                    f"blob for {update['ref']} is not a JSON record: {exc}"
                ) from exc
            continue
        raise Malformed(
            f"update for {update.get('ref')} must carry a JSON blob of at most "
            f"{MAX_BLOB} bytes, or delete: true"
        )
    return expires, leases, updates


def detail(exc):
    return exc.stderr.decode("utf-8", "replace").strip() if exc.stderr else str(exc)


def apply(commit, remote, now):
    try:
        message = git("log", "-1", "--format=%B", commit).decode("utf-8", "replace")
        expires, leases, updates = validate(parse(message))
    except subprocess.CalledProcessError as exc:
        print(f"queue-relay: cannot read {commit}: {detail(exc)}", file=sys.stderr)
        return MALFORMED
    except Malformed as exc:
        print(f"queue-relay: refusing {commit}: {exc}", file=sys.stderr)
        return MALFORMED
    touched = ", ".join(u["ref"] for u in updates)
    if now > expires:
        print(
            f"queue-relay: {commit} expired at {expires} ({now - expires}s ago); its client "
            f"has stopped waiting, so {touched} is left as it is",
            file=sys.stderr,
        )
        return EXPIRED
    push = ["push", "--quiet", "--atomic"]
    push += [f"--force-with-lease={ref}:{old}" for ref, old in leases]
    push.append(remote)
    try:
        for update in updates:
            if update.get("delete") is True:
                push.append(f":{update['ref']}")
            else:
                sha = git("hash-object", "-w", "--stdin", stdin=update["blob"].encode())
                push.append(f"{sha.decode().strip()}:{update['ref']}")
    except subprocess.CalledProcessError as exc:
        print(f"queue-relay: cannot store a blob of {commit}: {detail(exc)}", file=sys.stderr)
        return MALFORMED
    result = subprocess.run(["git", *push], capture_output=True, check=False)
    if result.returncode != 0:
        print(f"queue-relay: {remote} refused {touched}: {detail(result)}", file=sys.stderr)
        return REFUSED
    print(f"queue-relay: relayed {touched} to {remote}")
    return LANDED


def sweep(remote, now, grace):
    try:
        listed = git("ls-remote", remote, f"{REQUESTS}*").decode()
    except subprocess.CalledProcessError as exc:
        print(f"queue-relay: cannot list requests on {remote}: {detail(exc)}", file=sys.stderr)
        return 1
    status = 0
    for line in listed.splitlines():
        fields = line.split()
        if len(fields) != 2 or not fields[1].startswith(REQUESTS):
            continue
        ref = fields[1]
        leaf = ref.rsplit("/", 1)[-1]
        stamp = leaf.split(".", 1)[0]
        if not stamp.isdigit() or int(stamp) + grace >= now:
            continue
        done = subprocess.run(
            ["git", "push", "--quiet", remote, f":{ref}"], capture_output=True, check=False
        )
        if done.returncode == 0:
            print(f"queue-relay: swept {ref[len('refs/heads/'):]} (expired at {stamp})")
        else:
            status = 1
            print(f"queue-relay: could not sweep {ref}: {detail(done)}", file=sys.stderr)
    return status


def take(args, flag, convert):
    if flag not in args:
        return None
    at = args.index(flag)
    if at + 1 >= len(args):
        raise ValueError(f"{flag} needs a value")
    value = convert(args[at + 1])
    del args[at : at + 2]
    return value


def main(argv):
    args = list(argv)
    try:
        remote = take(args, "--remote", str) or "origin"
        now = take(args, "--now", int)
        grace = take(args, "--grace", int)
    except ValueError as exc:
        print(f"queue-relay: {exc}\n{USAGE}", file=sys.stderr)
        return MALFORMED
    now = int(time.time()) if now is None else now
    if args == ["--sweep"]:
        return sweep(remote, now, 60 if grace is None else grace)
    if len(args) != 1 or args[0].startswith("-"):
        print(USAGE, file=sys.stderr)
        return MALFORMED
    return apply(args[0], remote, now)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
