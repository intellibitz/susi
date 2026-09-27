# SUSI vision

SUSI is the operating layer for AI ecosystems: one governed substrate for
running, connecting, and supervising local and cloud models, agents, and tools.

It is not a replacement for Linux, macOS, or Windows. It runs on the host OS
and gives AI workloads the contracts an operating layer should provide:
discovery, admission, placement, identity, capability control, lifecycle,
coordination, evidence, audit, observability, and recovery.

This document states direction. Current product truth is defined by
`.agents/identity.json`, verified claims by `.agents/evidence.json`, and
enforceable crate/runtime boundaries by `ARCHITECTURE.md`. A vision item is
not evidence that a feature exists.

## North star

An operator should be able to give SUSI an intent without first deciding:

- which local or cloud model should execute it;
- which agent framework or peer should own it;
- which MCP, CLI, HTTP, or Wasm tool should be mounted;
- how local data, credentials, cost, and policy should constrain execution;
- how the result will be verified, attributed, and audited.

SUSI should make those decisions through inspectable policy and return a
result whose execution path can be explained and verified. Automation remains
bounded by operator policy, available capabilities, and evidence. Missing
credentials, tools, models, or proof must produce an honest failure—not a
fabricated success.

## One layer, two execution planes

### Local AI plane

SUSI should be the default operating layer for AI resources on one machine or
trusted local network:

- discover and admit local inference runtimes and model artifacts;
- select CPU/GPU placement from measured hardware and model readiness;
- load, reuse, evict, unload, verify, install, and remove managed models;
- mount local MCP servers, CLI agents, framework adapters, and Wasm reflexes;
- keep private work local when `local_only` or an equivalent constraint is set;
- maintain workspace-scoped memory, evidence, traces, and transactions;
- expose stable authenticated host endpoints and deterministic lifecycle
  commands.

Local execution is not automatically preferable. It must be ready, capable,
policy-compliant, and sufficiently resourced for the request.

### Cloud AI plane

SUSI should provide the same operating contracts for explicitly configured
remote resources:

- OpenAI-compatible, Anthropic, Gemini, OpenRouter, and future provider
  adapters;
- remote MCP servers and A2A peers;
- managed external agent and framework processes;
- capability and privacy admission before data leaves the host;
- cost, rate, residency, and organization-policy constraints;
- bounded network I/O, failover, cooldown, cancellation, and honest errors;
- the same evidence, attribution, placement, and audit semantics used locally.

SUSI never invents credentials, purchases access, or treats a configured name
as proof that a remote service is available.

### Shared decision plane

Local and cloud execution must not become two unrelated products. They share:

1. `CapabilityRegistry` admission and discovery.
2. MAC/privacy policy and explicit egress consent.
3. Hardware, capability, readiness, cost, and request constraints.
4. Evidence receipts and absolute-truth completion gates.
5. Correlated telemetry and signed placement/audit records.
6. Bounded concurrency, timeouts, cancellation, and resource accounting.
7. Extension packs for vendor-specific catalogs and configuration.

Every executed inference plan should identify whether placement was local,
cloud, or explicit, and carry a decision id that resolves to its signed audit
record. Policy simulation and execution should use the same routing logic.

## Agent-of-agents substrate

SUSI's unit of orchestration is a governed capability, not a particular model
vendor or agent framework. Models, tools, agents, and peers may be composed
into a swarm when parallel exploration or independent verification adds value.

The swarm layer should:

- select participants by declared capability, readiness, trust, and policy;
- run independent work concurrently where dependencies allow;
- preserve a pinned electorate for decisions requiring quorum;
- refuse mission completion when required evidence is absent;
- retain operator vetoes, budgets, leases, and cancellation;
- surface disagreement and partial failure rather than hiding it behind a
  synthesized answer;
- persist enough state to diagnose or resume supported workflows without
  claiming universal exactly-once recovery.

Single-agent or deterministic local execution remains valid when a swarm would
add cost without improving confidence.

## Trust model

SUSI is governance-first, but it is not a universal security boundary.

- Host-local secrets, API keys, and bearer tokens must be redacted before
  durable telemetry.
- Network-facing APIs require authentication except documented liveness and
  preflight routes.
- `local_only` must cover every known off-host capability, including remote
  agents whose names do not look like URLs.
- Untrusted Wasm runs through the Wasmer sandbox; optional shell isolation
  depends on Docker and explicit capability grants.
- The HMAC audit chain is tamper-evident under the host key. It does not make a
  compromised host or stolen key trustworthy.
- Cluster membership currently inherits the documented shared-key/LAN trust
  boundary and must not be marketed as zero-trust multi-tenant federation.
- Models may propose or review; they do not certify facts. Mission truth comes
  from live receipts, compiled reads, or native verified observations.

## Verified baseline

As of the evidence ledger's latest verification, SUSI has:

- a persistent daemon and stable authenticated host-contract ports;
- local/cloud inference routing with request constraints and auditable
  placement correlation;
- model discovery, managed lifecycle, loaded-model reporting, idle unloading,
  and pressure-driven LRU eviction for supported local runtimes;
- MCP, provider, external-agent, framework, and peer admission surfaces;
- evidence-gated mission completion and an inspectable blackboard/trace path;
- a cross-process serialized HMAC audit chain with fail-closed verification;
- Wasmer execution and optional Docker-backed command sandboxing;
- local-only egress enforcement across the registered remote tool classes;
- bounded public connections, handshakes, headers, response bodies, external
  processes, and host probes;
- cross-process-safe receipt archives and semantic-index persistence.

This list is a summary, not independent proof. Exact evidence and limitations
remain in `.agents/evidence.json` and `.agents/identity.json`.

## Next horizon: 0.15 local + cloud federation

The next release line should deepen the operating-layer contract rather than
grow disconnected catalogs.

### Placement and lifecycle

- Make placement decisions explainable as a structured constraint evaluation,
  not only a selected target and reason string.
- Feed measured latency, reliability, quality, energy, and spend into routing
  without allowing self-reported provider claims to become truth.
- Extend managed load/unload/health contracts across supported local runtimes.
- Make policy simulation capable of proving that execution and preview select
  the same candidate set.

### Federation

- Narrow the residual shared-cluster-key trust window with stronger per-node
  identity and transport guarantees.
- Make membership, rekey, anti-entropy, and recovery states visible through one
  operator surface.
- Add explicit federation conformance tests for mixed versions, partitions,
  replay, stale membership, and recovery.
- Preserve local operation when every remote peer is absent or refused.

### Evidence and recovery

- Make durable workflow coverage explicit per workflow type.
- Bind more state transitions to typed receipts and correlation ids.
- Provide supported repair/export procedures for damaged local ledgers without
  silently rewriting history.
- Keep derived indexes rebuildable from authoritative stores.

### Ecosystem

- Define versioned conformance contracts for providers, MCP servers, agents,
  and extension packs.
- Prefer protocol admission over new vendor-specific core branches.
- Publish compatibility status from executable probes, not static marketing
  counts.
- Keep optional foreign runtimes behind Rust-owned adapters and policy gates.

## Release gates

A capability belongs in current-product documentation only when all applicable
gates hold:

1. A real execution path exists; no stub or simulated-success path stands in.
2. Failure is typed and observable.
3. Security, privacy, and credential boundaries are documented and tested.
4. Concurrency and external I/O are bounded or explicitly documented as a
   user-controlled long-running operation.
5. State writes are crash-safe in proportion to their authority.
6. Tests exercise the claimed behavior, including a relevant failure path.
7. Evidence records the implementation anchor and verification performed.
8. README, identity, architecture, and runtime behavior agree.

Scale, latency, uptime, productivity, compliance, adoption, and industry-impact
claims require reproducible measurements or external evidence. Until then they
remain research goals and must not be phrased as current SUSI capabilities.

## Non-goals

SUSI does not aim to:

- replace the host kernel, init system, package manager, or desktop;
- guarantee that every task should use a swarm;
- expose secrets or private model internals in the name of transparency;
- claim universal protocol, model-format, cloud, or framework support;
- promise exactly-once execution across arbitrary crashes and partitions;
- certify legal, regulatory, medical, financial, or security compliance merely
  because audit or policy primitives exist;
- claim production scale, uptime, market adoption, or productivity outcomes
  without current evidence.

The enduring vision is narrower and more useful: make heterogeneous AI
capabilities operate as one inspectable, policy-governed, evidence-bound system
across the local machine and the cloud.
