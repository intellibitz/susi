# GMCP completion roadmap

Status: **execution program established; all VC-203 vectors are open**
Baseline date: **2026-10-17**
Current protocol target: **MCP 2026-07-28**
Compatibility target: **MCP 2025-11-25 plus explicitly isolated older adapters**

## Outcome

GMCP is Susi's end-to-end local Model Context Protocol environment. It is complete
only when it can author, host, consume, aggregate, secure, observe, test, package,
and operate MCP servers and clients without a second control plane.

"Perfect" is used here as a measurable release property, not an unbounded promise:

1. every applicable requirement known at the qualification cut is classified;
2. every classified requirement traces to production code, positive and negative
   tests, and retained evidence;
3. all declared compatibility, security, reliability, performance, platform, and
   usability gates pass without unexplained exceptions; and
4. standards drift or a newly accepted metric invalidates the qualification
   certificate until it is classified and covered.

This makes the program finite at any release cut while preventing "complete" from
going stale. Roadmap prose, task count, mocks, and green happy-path tests are never
sufficient evidence by themselves.

## Scope

The program covers the whole MCP lifecycle:

- protocol models, JSON-RPC behavior, versioning, transports, primitives,
  notifications, subscriptions, MRTR, tasks, Apps, authorization, and extensions;
- MCP server authoring, client/host behavior, gateway aggregation, routing,
  composition, adapters, registry use, package/runtime management, and desired
  state;
- consent, policy, credentials, sandboxing, supply chain, content and network
  security, identity isolation, audit, privacy, and provenance;
- supervision, observability, recovery, offline use, performance, cross-platform
  distribution, inspection, record/replay, conformance, interoperability, fuzzing,
  red-team, documentation, release evidence, and SLOs.

Work unrelated to MCP is out of scope unless it is a dependency of an MCP
production path. Experimental protocol ideas may be explored, but they cannot be
advertised as standard MCP and do not satisfy a normative requirement.

## Authoritative baseline

The requirements ledger in VC-203-001 must pin exact revisions and content hashes.
At this baseline the primary authorities are:

- [MCP 2026-07-28 schema](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2026-07-28/schema.ts)
  and [specification](https://modelcontextprotocol.io/specification/2026-07-28);
- [MCP 2026-07-28 changelog](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/changelog.mdx)
  and [version negotiation policy](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/versioning.mdx);
- [official conformance suite](https://github.com/modelcontextprotocol/conformance);
- [official Registry API](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/api/official-registry-api.md),
  [server.json format](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/server-json/generic-server-json.md),
  and [registry requirements](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/official-registry-requirements.md);
- [MCP Apps](https://apps.extensions.modelcontextprotocol.io/),
  [Tasks](https://tasks.extensions.modelcontextprotocol.io/specification/2026-07-28/tasks),
  and [authorization extensions](https://github.com/modelcontextprotocol/ext-auth);
- [official client feature matrix](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/extensions/client-matrix.mdx);
- [MCP security best practices](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/docs/2026-07-28/tutorials/security/security_best_practices.mdx),
  [local-server security](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/docs/2026-07-28/tutorials/security/local-server-security.mdx),
  [OWASP MCP Security Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/MCP_Security_Cheat_Sheet.html),
  and [OWASP MCP Top 10](https://owasp.org/projects/mcp-top-10);
- [official MCP Inspector guidance](https://github.com/modelcontextprotocol/docs/blob/main/docs/tools/inspector.mdx).

A mirror, blog post, SDK behavior, or host implementation may add an interoperability
requirement, but cannot override normative specification text.

## What changed in the current protocol generation

The 2026-07-28 baseline is not a cosmetic revision. The ledger and implementation
must explicitly account for these changes:

- protocol sessions and the MCP session header are removed from the modern model;
- initialize/initialized is replaced by per-request version and capability
  metadata, with server information returned on results;
- server/discover is mandatory;
- subscriptions/listen replaces HTTP GET and legacy resource subscribe/unsubscribe;
- cross-call state uses explicit principal-bound handles;
- MRTR replaces server-initiated roots, sampling, and elicitation flows;
- Tasks is an official extension with server-directed lifecycle methods;
- ping, logging/setLevel, roots/list_changed, SSE resumability, and legacy context
  flags are removed or deprecated on the modern path;
- resultType, cache metadata, deterministic listing, standard trace context, and
  method/name headers are part of the observable contract;
- JSON Schema 2020-12 and the current OAuth issuer, audience, client metadata, and
  resource-server rules are required.

Legacy behavior remains available only through explicit version adapters with
downgrade telemetry and removal policy.

## Audited starting point

This audit is a baseline, not a delivery verdict.

| Area | Present baseline | Material work still required |
|---|---|---|
| Server protocol | `crates/susi-gmcp/src/protocol.rs` implements current discovery, core primitives, modern subscriptions, MRTR, tasks, schema checks, and older request handling | normative ledger, exhaustive edge cases, true cross-surface state isolation, official suites, and retained evidence |
| Transports | bounded stdio and Streamable HTTP, local auth, origin/host checks, rate and concurrency controls, TLS for remote use | complete client/server semantics, legacy isolation, HTTP/2/proxy/drain testing, common adapter harness, and platform parity |
| Server primitives | tools, resources, templates, prompts, completion, progress, notifications, and structured outputs have substantial tests | full metadata/cache/filter matrices, adversarial schemas/content, authorization re-checks, durability, and differential coverage |
| Client | `susi-vendor-mcp` connects over stdio or HTTP and lists/calls tools | resources, prompts, completion, subscriptions, callbacks, MRTR, tasks, Apps, auth, cancellation, full host UX, and reference interop |
| Ecosystem | curated leading servers, a static registry, configuration import, health checks, and runtime hot-plugging exist | official Registry lifecycle, trustworthy package resolution, transactional runtime management, desired state, catalog reconciliation, and publication |
| Security | bearer/peer controls, untrusted-output policy, data classification, sandbox pieces, and signed audit foundations exist | unified policy choke point, OAuth roles, secret store, OS sandbox profiles, supply-chain proof, injection/rug-pull/SSRF corpora, and tenant isolation |
| Operations | daemon integration, status/doctor commands, process launch, rate limits, and audit infrastructure exist | durable supervisor, recovery, OpenTelemetry schema, SLOs, capacity/fairness gates, offline restore, cross-platform packaging, and laboratory tooling |
| Verification | unit/integration tests and a previous lifecycle probe exist | pinned official conformance in both roles, extension suites, reference SDK matrix, fuzz/model checking, red-team, platform/performance/recovery gates, and independent qualification |

## Completion invariants

Every VC-203 implementation must preserve these invariants:

- **One semantic core.** Transports, CLI, daemon, gateway, SDK, and tests call the
  same production protocol and policy paths.
- **Deny by default.** Discovery is not trust, installation is not enablement,
  content is not authority, and consent cannot be supplied by server-controlled
  text.
- **No ambient authority.** Identity, tenant, server, capability, state handle,
  credential, network destination, and policy epoch remain explicit.
- **Bound everything.** Bytes, schemas, recursion, processes, connections, queues,
  subscriptions, retries, time, CPU, memory, disk, logs, traces, fan-out, and cost
  have enforced limits.
- **No silent downgrade.** Version, transport, auth, extension, sandbox, and
  platform degradation is explicit, policy-controlled, observable, and tested.
- **Exactly-once claims require evidence.** Side effects, retries, durable tasks,
  recovery, and gateway composition use idempotency or transaction/compensation
  semantics; labels alone do not create guarantees.
- **Untrusted means untrusted.** Tool descriptions, schemas, prompts, resources,
  UI, errors, and outputs cannot grant permission or reveal protected context.
- **Local first and offline capable.** Network use is declared egress, and retained
  artifacts are sufficient to boot, inspect, verify, restore, and run declared
  local capabilities offline.
- **Compatibility is bidirectional.** GMCP is tested as server and client against
  reference implementations, through direct and gateway paths.
- **Evidence is reproducible.** A clean machine can reconstruct every scorecard
  claim from pinned sources, test inputs, artifacts, and hashes.

## Task lattice

Each vector has seven dependency-ordered tasks, for **336 open implementation
tasks** in this baseline:

| Suffix | Work package | Required output |
|---|---|---|
| 01 | Contract | clause mapping, API/data model, threat model, fixtures, budgets, and evidence locations |
| 02 | Production path | canonical state and primary implementation with no stub or parallel bypass |
| 03 | Integration | all applicable server, client, host, gateway, CLI/config, callback, migration, and lifecycle surfaces |
| 04 | Adversarial behavior | validation, authorization, isolation, bounds, cancellation, concurrency, fault, and recovery tests |
| 05 | Interoperability and quality | official/reference interop, differential/property tests, platform scope, and performance budgets |
| 06 | Operations and evidence | diagnostics, observability, docs, examples, runbooks, upgrade/rollback, and retained artifacts |
| M | Mastery | one production-path gate that consumes 01-06 evidence and proves the roadmap mastery target |

Requirement identifiers are immutable: `GMCP-203-NNN-01` through
`GMCP-203-NNN-06`, plus `GMCP-203-NNN-M`. Every task has one unique
identifier and one non-zero-test acceptance command. The mastery task depends on
all six work packages. A vector's contract task depends on the mastery tasks of
its prerequisite vectors.

VC-203-048 is the terminal gate. Its mastery task directly depends on every other
vector mastery task, and `gmcp_complete_mastery` must fail if any requirement,
evidence artifact, matrix cell, risk disposition, or upstream change is missing.

## Vector catalog

| Vector | Priority | Type | Capability | Prerequisites |
|---|---|---|---|---|
| VC-203-001 | P0 | GOVERNANCE | Normative requirements ledger and standards drift | — |
| VC-203-002 | P0 | CORE | JSON-RPC, protocol metadata, errors, cancellation, and negotiation | VC-203-001 |
| VC-203-003 | P0 | CORE | Modern stateless lifecycle and server discovery | VC-203-002 |
| VC-203-004 | P1 | MEMORY_AND_ECOSYSTEM | Legacy lifecycle compatibility and governed deprecation | VC-203-002, VC-203-003 |
| VC-203-005 | P0 | LOCAL_HOSTING | Production-grade stdio transport | VC-203-002, VC-203-003 |
| VC-203-006 | P0 | LOCAL_HOSTING | Production-grade Streamable HTTP transport | VC-203-002, VC-203-003 |
| VC-203-007 | P1 | CORE | Transport adapter SPI and embedded/in-process operation | VC-203-005, VC-203-006 |
| VC-203-008 | P0 | CORE | Complete schemas, primitives, content, and metadata model | VC-203-001, VC-203-002 |
| VC-203-009 | P0 | CORE | Complete tool server surface | VC-203-003, VC-203-008 |
| VC-203-010 | P0 | CORE | Complete resource server surface | VC-203-003, VC-203-008 |
| VC-203-011 | P0 | CORE | Complete prompt and completion server surface | VC-203-003, VC-203-008 |
| VC-203-012 | P0 | CORE | Subscriptions, notifications, progress, and operational messages | VC-203-005, VC-203-006, VC-203-009, VC-203-010, VC-203-011 |
| VC-203-013 | P0 | CORE | MRTR, elicitation, sampling, and roots interaction model | VC-203-003, VC-203-008 |
| VC-203-014 | P0 | CORE | Official Tasks extension and durable long-running work | VC-203-003, VC-203-013, VC-203-016 |
| VC-203-015 | P1 | MEMORY_AND_ECOSYSTEM | MCP Apps extension and secure interactive UI | VC-203-009, VC-203-010, VC-203-016, VC-203-019 |
| VC-203-016 | P0 | GOVERNANCE | First-class extension framework and extension governance | VC-203-001, VC-203-003, VC-203-008 |
| VC-203-017 | P0 | CORE | Complete MCP client and host | VC-203-004, VC-203-009, VC-203-010, VC-203-011, VC-203-012, VC-203-013, VC-203-016 |
| VC-203-018 | P0 | CORE | Client connection runtime, caching, retries, and concurrency | VC-203-005, VC-203-006, VC-203-017 |
| VC-203-019 | P0 | GOVERNANCE | Human consent, approvals, and interaction UX | VC-203-013, VC-203-017, VC-203-024 |
| VC-203-020 | P1 | MEMORY_AND_ECOSYSTEM | GMCP server authoring SDK and plugin lifecycle | VC-203-008, VC-203-009, VC-203-010, VC-203-011, VC-203-013, VC-203-016 |
| VC-203-021 | P0 | FEDERATION | Local MCP gateway, reverse proxy, and aggregation plane | VC-203-017, VC-203-018, VC-203-024 |
| VC-203-022 | P0 | FEDERATION | Namespacing, routing, composition, and virtual capabilities | VC-203-009, VC-203-010, VC-203-011, VC-203-012, VC-203-021 |
| VC-203-023 | P2 | MEMORY_AND_ECOSYSTEM | Service adapters and generated MCP surfaces | VC-203-020, VC-203-022 |
| VC-203-024 | P0 | GOVERNANCE | Unified policy engine and least-privilege authorization | VC-203-017 |
| VC-203-025 | P0 | GOVERNANCE | OAuth-protected MCP resource server | VC-203-006, VC-203-024 |
| VC-203-026 | P0 | GOVERNANCE | OAuth-capable MCP client and authorization flow | VC-203-017, VC-203-025 |
| VC-203-027 | P1 | GOVERNANCE | Enterprise and machine-to-machine authorization extensions | VC-203-016, VC-203-025, VC-203-026 |
| VC-203-028 | P0 | GOVERNANCE | Secrets, credentials, and cryptographic lifecycle | VC-203-024, VC-203-025, VC-203-026, VC-203-027 |
| VC-203-029 | P0 | LOCAL_HOSTING | Local process, filesystem, and network sandboxing | VC-203-005, VC-203-024, VC-203-028 |
| VC-203-030 | P0 | GOVERNANCE | Supply-chain integrity and server/package trust | VC-203-001, VC-203-028, VC-203-029 |
| VC-203-031 | P0 | GOVERNANCE | Tool poisoning, prompt injection, rug-pull, and exfiltration defenses | VC-203-009, VC-203-017, VC-203-019, VC-203-024, VC-203-030 |
| VC-203-032 | P0 | GOVERNANCE | Transport, origin, SSRF, DNS, and endpoint security | VC-203-006, VC-203-024, VC-203-025, VC-203-026, VC-203-028, VC-203-031 |
| VC-203-033 | P0 | GOVERNANCE | Identity, tenancy, state-handle, and data isolation | VC-203-024, VC-203-025, VC-203-026, VC-203-027, VC-203-032 |
| VC-203-034 | P0 | OPERATIONS | Audit, privacy, provenance, and compliance evidence | VC-203-024, VC-203-031, VC-203-033 |
| VC-203-035 | P1 | MEMORY_AND_ECOSYSTEM | Official MCP Registry discovery, validation, and publication | VC-203-001, VC-203-030 |
| VC-203-036 | P0 | LOCAL_HOSTING | Package and runtime install, update, rollback, and removal | VC-203-029, VC-203-030, VC-203-035 |
| VC-203-037 | P1 | CONFIGURATION | Configuration import, export, migration, and desired state | VC-203-017, VC-203-028, VC-203-036 |
| VC-203-038 | P1 | MEMORY_AND_ECOSYSTEM | Catalog, discovery, health, compatibility, and trust intelligence | VC-203-017, VC-203-030, VC-203-035, VC-203-036, VC-203-037 |
| VC-203-039 | P0 | OPERATIONS | Server supervision and lifecycle orchestration | VC-203-018, VC-203-029, VC-203-036, VC-203-038 |
| VC-203-040 | P1 | OPERATIONS | OpenTelemetry, metrics, logs, traces, and diagnostics | VC-203-034, VC-203-039 |
| VC-203-041 | P0 | OPERATIONS | Reliability, durability, recovery, backup, and offline operation | VC-203-014, VC-203-018, VC-203-033, VC-203-039, VC-203-040 |
| VC-203-042 | P1 | OPERATIONS | Performance, capacity, fairness, and resource budgets | VC-203-018, VC-203-039, VC-203-040, VC-203-041 |
| VC-203-043 | P1 | LOCAL_HOSTING | Cross-platform packaging, service integration, and distribution | VC-203-005, VC-203-006, VC-203-007, VC-203-029, VC-203-036, VC-203-039 |
| VC-203-044 | P1 | MEMORY_AND_ECOSYSTEM | Inspector, debugger, record/replay, mocks, and test laboratory | VC-203-017, VC-203-019, VC-203-040, VC-203-043 |
| VC-203-045 | P0 | MEMORY_AND_ECOSYSTEM | Official conformance and ecosystem interoperability matrix | VC-203-004, VC-203-012, VC-203-013, VC-203-014, VC-203-015, VC-203-017, VC-203-025, VC-203-026, VC-203-027, VC-203-043, VC-203-044 |
| VC-203-046 | P0 | RSI_EVALUATION | Property testing, fuzzing, model checking, and continuous red-team | VC-203-024, VC-203-029, VC-203-030, VC-203-031, VC-203-032, VC-203-033, VC-203-034, VC-203-045 |
| VC-203-047 | P1 | GOVERNANCE | Documentation, examples, developer experience, release quality, and SLOs | VC-203-020, VC-203-036, VC-203-043, VC-203-044, VC-203-045, VC-203-046 |
| VC-203-048 | P0 | GOVERNANCE | Independent GMCP qualification and continuously complete release gate | VC-203-001, VC-203-002, VC-203-003, VC-203-004, VC-203-005, VC-203-006, VC-203-007, VC-203-008, VC-203-009, VC-203-010, VC-203-011, VC-203-012, VC-203-013, VC-203-014, VC-203-015, VC-203-016, VC-203-017, VC-203-018, VC-203-019, VC-203-020, VC-203-021, VC-203-022, VC-203-023, VC-203-024, VC-203-025, VC-203-026, VC-203-027, VC-203-028, VC-203-029, VC-203-030, VC-203-031, VC-203-032, VC-203-033, VC-203-034, VC-203-035, VC-203-036, VC-203-037, VC-203-038, VC-203-039, VC-203-040, VC-203-041, VC-203-042, VC-203-043, VC-203-044, VC-203-045, VC-203-046, VC-203-047 |

The full, normative mastery text is in [`.agents/roadmap.json`](../.agents/roadmap.json).
Task-level detail lives in [`.agents/tasks/`](../.agents/tasks/). The structural
checker is [`scripts/check-gmcp-roadmap.py`](../scripts/check-gmcp-roadmap.py).

## Execution waves

The dependency graph permits parallel work without turning the plan into a flat
backlog.

1. **Truth and protocol:** VC-203-001 through VC-203-008 establish the ledger,
   protocol core, lifecycle, transports, and lossless data model.
2. **MCP behavior:** VC-203-009 through VC-203-016 complete server primitives,
   interaction models, official extensions, and extension governance.
3. **Host and composition:** VC-203-017 through VC-203-023 deliver the full host,
   resilient client runtime, consent, authoring SDK, gateway, routing, composition,
   and generated adapters.
4. **Trust boundary:** VC-203-024 through VC-203-034 close policy, OAuth,
   credentials, sandbox, supply chain, injection/exfiltration, network, tenant,
   privacy, audit, and evidence gaps.
5. **Ecosystem and operations:** VC-203-035 through VC-203-044 deliver Registry,
   package/runtime lifecycle, desired state, catalog intelligence, supervision,
   telemetry, recovery, capacity, distribution, and the test laboratory.
6. **Proof and release:** VC-203-045 through VC-203-048 run official conformance,
   ecosystem interop, fuzz/model/red-team programs, executable documentation, SLOs,
   and independent qualification.

P0 does not mean "ignore prerequisites." It means that once dependencies are green,
P0 work blocks a complete release. P1 is required for the same final certificate;
P2 is ordered later because it expands breadth, not because it is optional.

## Qualification scorecard

The terminal scorecard must report raw counts, denominators, artifact hashes, and
waivers rather than a single opaque percentage.

| Dimension | Release threshold |
|---|---|
| Requirements | 100% of applicable normative clauses and accepted local requirements traced; zero unclassified rows |
| Tasks and vectors | every VC-203 task closed by a non-zero production-path test; every mastery gate green |
| Official conformance | client and server, all supported revisions/transports/extensions, zero expected or unexplained failures |
| Interoperability | every declared SDK/host/server/platform matrix cell green or removed from the support claim |
| Security | zero unresolved critical/high findings; every threat-model abuse case automated; secret/redaction canaries green |
| Supply chain | all shipped and installable artifacts pinned, verified, scanned, SBOM-attested, and rollbackable |
| Reliability | declared RPO/RTO, restart, kill, corruption, full-disk, clock, cancellation, and offline restore drills green |
| Performance | all published p50/p95/p99, throughput, startup, resource, fairness, and overload budgets green |
| Privacy and audit | action coverage complete, payload/secret exclusion canaries green, retention/deletion/export verified |
| Platforms | every advertised OS/architecture/privilege/service-manager cell green on a clean machine |
| Documentation and UX | executable examples green; setup, diagnosis, migration, recovery, and accessibility studies meet targets |
| Drift | upstream source hashes current; zero unclassified spec, extension, conformance, registry, security, or accepted-metric changes |

A waiver cannot make the scorecard green. It can only narrow a public support claim,
must name an owner and expiry, and must be reflected in discovery, documentation,
and package metadata.

## Standards and metric drift loop

VC-203-001 and VC-203-048 run the following loop on schedule and before release:

1. fetch pinned authoritative sources and record content hashes;
2. diff protocol schema, normative prose, conformance scenarios, official
   extensions, Registry contracts, security guidance, and the client matrix;
3. classify each change as applicable, incompatible, superseded, experimental, or
   not applicable with evidence;
4. generate or update requirement rows and implementation tasks;
5. invalidate affected evidence and the qualification certificate;
6. re-run the smallest affected gates, then the complete qualification command;
7. publish the new matrix and signed scorecard.

Human- or AI-proposed metrics enter through the same governed intake: define the
measurable property, workload, threshold, reproducibility method, gaming/refutation
test, and owner. Accepted metrics become requirements; undefined opinions do not
pretend to be testable standards.

## Evidence model

The final evidence graph is:

`source clause or accepted metric -> requirement ID -> roadmap vector -> task ->
production path -> positive/negative test -> immutable artifact -> mastery gate ->
qualification scorecard`

Every arrow is machine-checked. Evidence records include source revision/hash,
build and platform identity, exact command, inputs/seeds, outcome, duration,
resource/cost observations, artifact hashes, and redaction status. Expired,
unreproducible, locally modified, or unsigned evidence is not green.

## Maintaining and executing the queue

Use the normal Susi workflow; do not batch-close roadmap work:

```bash
susi workflow check
susi tasks roadmap --uncovered
susi tasks list
susi tasks claim <task-id> --scope <exact-path>
# implement one requirement and run its acceptance command
susi tasks close <task-id>
```

Validate roadmap structure at any time:

```bash
python3 scripts/check-gmcp-roadmap.py
cargo test --locked --test gmcp_roadmap
```

The checker validates vector coverage, unique requirement IDs, dependency integrity,
acyclic ordering, acceptance commands, mastery fan-in, terminal fan-in, and
documentation coverage. It deliberately does not mark capability delivered; only
the requirement and mastery tests can do that.
