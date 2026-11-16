# Documentation index

Every document under `docs/` is listed here, with the main documents elsewhere
in the repository. Look here before you add one: extend the document that already covers your topic, and add a line here
for a new one in the same change. The rules agents work by are in
[`AGENTS.md`](../AGENTS.md); they are not repeated here.

## Architecture and contracts

- [Susi architecture: the socket, its pillars and what sits on it](architecture.md)
- [Threat model: the open core socket](threat-model.md)
- [Host contract conformance](host-contract-conformance.md)
- [Integrating with Susi](integrating-with-susi.md)
- [Extension pack manifest schema & 5-minute starter guide](extension-pack.md)
- [The sealed peer channel](peer-channel.md)
- [Agent-operated local Windows channels](local-channels.md)
- [Table-free Rust identifier classification](identifier-platform.md)
- [Unicode platform boundary](unicode-platform.md)
- [Lean core: complete standards socket below 100,000 lines](lean-core.md)
- [Native Rust Susi: delivery contract and evidence](native-rust-direction.md)
- [Step 1: Susi builds, tests, improves and uses itself](step1-acceptance.md)
- [Provider-neutral qualification inputs](provider-neutral-inputs.md)
- [GMCP completion roadmap](gmcp-roadmap.md)

## Harness and agents

- [> **Current governing contract:** follow `AGENTS.md` and the mandates in `.agents/identity.json`.](harness.md)
- [SUSI harness as an AI OS](harness-os.md)
- [Harness coordination across hosts: the repository is the shared state](harness-coordination.md)
- [Harness runbook](harness-runbook.md)
- [Harness FAQ](harness-faq.md)
- [Recursive driver/engine delegation: first slice](recursive-delegation.md)
- [Isolated cloud agent sessions](cloud-sessions.md)
- [Agent refs: `refs/agents/<agent>`](agent-refs.md)

## Building, evidence and releases

- [Native development bootstrap](native-bootstrap.md)
- [Evidence: verification and qualification](evidence.md)
- [Gate receipts](gate-receipts.md)
- [Parallel development without weakening landing evidence](parallel-development.md)
- [Parallel runbook](parallel-runbook.md)
- [Parallel FAQ](parallel-faq.md)
- [Harvest: `local-harvest`](harvest.md)
- [Release distillation: plan-only planner (first slice)](distillation.md)
- [Portable feature and release distillation](release-distillation.md)
- [Release lanes: many agents, one release, no conflicts](release-lanes.md)
- [Releases](releases.md)
- [Git storage publisher: first portable slice](git-storage-publisher.md)

## GMCP contracts

One contract per GMCP vector, each quoting its pinned specification clauses
verbatim (see the [GMCP completion roadmap](gmcp-roadmap.md)):

[203-001](gmcp/contract-203-001.md) · [203-002](gmcp/contract-203-002.md) · [203-003](gmcp/contract-203-003.md) · [203-004](gmcp/contract-203-004.md) · [203-005](gmcp/contract-203-005.md) · [203-006](gmcp/contract-203-006.md) · [203-007](gmcp/contract-203-007.md) · [203-008](gmcp/contract-203-008.md) · [203-009](gmcp/contract-203-009.md) · [203-010](gmcp/contract-203-010.md) · [203-011](gmcp/contract-203-011.md) · [203-012](gmcp/contract-203-012.md) · [203-013](gmcp/contract-203-013.md) · [203-014](gmcp/contract-203-014.md) · [203-016](gmcp/contract-203-016.md) · [203-017](gmcp/contract-203-017.md) · [203-020](gmcp/contract-203-020.md)

## Elsewhere in the repository

- [SUSI architecture](../ARCHITECTURE.md)
- [SUSI vision](../VISION.md)
- [Forking and contributing to Susi](../CONTRIBUTING.md)
- [Security Policy](../SECURITY.md)
- [Config layout](../config/README.md)
- [susi-harness](../crates/susi-harness/README.md)
- [Rust-native Wasm substrate](../crates/susi-vendor-wasmtime/README.md)
- [Original Rust Windows process boundary](../crates/susi-windows-runtime/README.md)
- [Windows product standard-library prototype](../toolchain/windows-stdlib/README.md)
- [Native product standard library and release checks](../toolchain/windows-stdlib/NATIVE-BUILDS.md)

## Dated records

Kept for reference. Each describes the state on its date, so check the
current documents and code before relying on it.

- [Historical workflow measurements](workflow-speed.md).
- [Independent agents, integration and pristine releases](ceo-agent-mandate.md),
  and the numbered directive documents `ceo-mandate-74.md` to
  `ceo-mandate-87.md` in this directory: superseded by the active contract in
  [`AGENTS.md`](../AGENTS.md).
