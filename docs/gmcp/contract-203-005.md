# VC-203-005 contract

This contract binds every normative clause of vector VC-203-005. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 20 pinned clauses below. Every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2026-07-28/basic/transports/stdio.mdx | 6fd49766c40dc093d1f5993ab584bad0a06cbb496c2d3019c50f8fe3c8171e57 | 13-144 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. Requests, responses and handles are explicit data, never ambient state.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2026-0728-basic-transports-stdio-mdx-0013 | must_not | - Messages are delimited by newlines, and **MUST NOT** contain embedded newlines. | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0014 | may | - The server **MAY** write UTF-8 strings to `stderr` for any logging purposes | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0016 | may | - The client **MAY** capture, forward, or ignore the server's `stderr` output and | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0017 | should_not | **SHOULD NOT** assume `stderr` output indicates error conditions. | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0018 | must_not | - The server **MUST NOT** write anything to its `stdout` that is not a valid MCP | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0020 | must_not | - The client **MUST NOT** write anything to the server's `stdin` that is not a | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0029 | should | built on such streams **SHOULD** reuse this framing and the message rules on | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0036 | must_not | to the server's `stdin`, one message per line. The client **MUST NOT** write | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0050 | must | [`subscriptions/listen`][subscriptions-listen] request. Clients **MUST** | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0055 | must_not | The server **MUST NOT** write JSON-RPC _requests_ to `stdout`. | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0078 | must | To cancel an in-flight request, the client **MUST** send a | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0081 | should | to close. Servers **SHOULD** stop work on a cancelled request as soon as | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0082 | must_not | practical and **MUST NOT** send any further messages for it. See | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0089 | should | The client **SHOULD** initiate shutdown by: | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0102 | should | Servers **SHOULD** exit promptly when their standard input is closed or reads | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0106 | may | The server **MAY** initiate shutdown by closing its output stream to the | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0111 | should | If the server process exits unexpectedly, the client **SHOULD** restart it. | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0124 | should | legacy version that requires an `initialize` handshake **SHOULD** probe with | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0139 | must_not | The fallback **MUST NOT** be keyed to one specific error code: legacy servers | crates/susi-gmcp/src/stdio.rs |
| MCP-2026-0728-basic-transports-stdio-mdx-0144 | should | probing is still **RECOMMENDED**: some legacy servers do not validate that a | crates/susi-gmcp/src/stdio.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: modern=17, optional=3. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy.

## Threat model

A malicious peer can replay a handle, answer another principal's request, or stall past a deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth.

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-005.jsonl, keyed by row id; the tests consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-005.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` is the production health check. It emits one bounded
  JSON report containing `rows`, `documents`, `revisions`, `dispositions`,
  `categories`, `vectors`, and `verified`; a false verification exits
  non-zero before protocol traffic is trusted.
- `susi mcp call susi ledger_verify {}` exposes the same report through the
  tool surface. Comparing it with the CLI report is a transport-independent
  smoke check for direct, daemon, gateway, and stdio dispatch.
- Served stdio sessions (`susi mcp serve`) operate with byte-accurate newline
  framing, bounded lines, stdout protocol purity, stderr diagnostics, and prompt
  reclamation on input pipe closure.
- `susi mcp ledger --drift --refetched <dir>` is the deterministic, air-gapped
  check. The directory is keyed by `sources.json` document paths and is bounded
  by the same source and artifact limits as the live check.

### Diagnostics and incident triage

Every drift finding has a stable `kind`: `hash_drift` means bytes differ from
the pinned source, while `unfetchable` means a required document is absent or
could not be retrieved. A non-zero command and `verified: false` are required
for either condition; callers must not continue with a partially checked
corpus. For a local failure, restore the named vendored file from git. For an
upstream `hash_drift`, preserve the report and begin the migration below. For
an `unfetchable` source, retry within the published bounds, then recover the
last known pin from git history rather than silently skipping it.
Protocol errors on stdio envelopes are strictly typed: malformed JSON earns
`-32700`, while non-2.0, batch arrays, or protocol violations earn `-32600`.
Diagnostic traces write strictly to `stderr` to preserve `stdout` wire purity.

### Incident triage

Treat a non-zero report as an incident until the named source or pin is
restored and the deterministic check is green again. Preserve the JSON report
and the corresponding evidence record with the incident for independent
reproduction.

### Migration (re-pinning the stdio transport specification)

1. Claim a task and record the upstream revision and reason for the re-pin.
2. Refresh `.agents/gmcp/spec/` and `sources.json` from that exact revision.
3. Run `scripts/check-gmcp-ledger.py --write`, review every changed
   stdio transport clause, and update fixtures, compatibility decisions, and the
   retained evidence record.
4. Run `susi mcp ledger`, the deterministic `--refetched` check, and the full
   GMCP acceptance gate before publishing the change.

### Rollback

The vendored specification, manifest, ledger, fixtures, and evidence are
committed artifacts. Roll back a bad re-pin with `git revert` of its commit,
then rerun the CLI and deterministic drift checks. This restores the prior
stdio transport semantics without a data migration; do not delete
evidence rows or rewrite a prior proof event.

