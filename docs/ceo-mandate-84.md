# CEO Mandate 84: Susi Core Stays Under 25,000 Lines

Effective 2026-11-16; limit tightened to 50,000 and then to 25,000 on 2026-11-18. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — on every change to
`intellibitz/susi`. `.agents/identity.json` records this directive as
**Mandate 84**. It turns the CEO's earlier goal, Vision `V-6` ("susi core must
be less than 100k lines. no functionalities must be dropped."), into a standing
rule, and it wins where older text differs (the lean-core program in
`AGENTS.md`, [`docs/lean-core.md`](lean-core.md), and the "measured reason" for
growth in Mandates 69 and 78). `intellibitz/susi-commercial-apps` carries the
paired commercial edition in its own `docs/ceo-mandate-84.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 4 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 84 (`docs/ceo-mandate-84.md`):
Susi core is always under 25,000 lines; until it is, core never grows, so pay
for every line you add to core by removing as many in the same change, lower the
pinned ceiling when you shrink it, put add-ons in `susi-commercial-apps`, and
never drop a capability or hide code to make the number.

## 0. The CEO's words

> make a mandate, susi core should always be under 100k lines.

The CEO tightened it twice on 2026-11-18, first:

> bring it down to less than 50k lines.

and then, setting the current limit:

> core under 25k

Recorded as vision `V-9` here and `V-10` in `intellibitz/susi-commercial-apps`. It
continues `V-6`, which the CEO set earlier the same day.

## 1. The rule

1. **The limit.** Susi core's complete socket is always strictly below
   25,000 physical lines: at most 24,999.
2. **What counts.** Every physical line (comments, blanks and inline tests
   included) of first-party runtime Rust, on every platform and every feature,
   plus the in-repository `susi-vendor-*` facades: all pillar, foundation,
   host, client and compatibility code, not only the crate named `susi-core`.
   This is the counting contract of Vision `V-6` in
   [`docs/lean-core.md`](lean-core.md), measured by `repository_size` in
   `tests/lean_core_ceilings.rs`. Test, bench and fuzz files, development
   tooling (`xtask/`, `toolchain/`), third-party `vendor/` source and `.agents/`
   evidence are measured and reported beside it, not counted in it.
3. **Until core is below the limit, it never grows.** The measured size is
   pinned as `COMPLETE_SOCKET_CEILING` in `tests/lean_core_ceilings.rs`. A
   change that would raise the count pays for itself in the same change by
   removing at least as many runtime lines from core (consolidating duplicate
   implementations behind existing contracts), or, when it is an add-on, ships
   as a plug in `intellibitz/susi-commercial-apps` and reaches core over the
   socket.
4. **The ceiling only goes down.** A change that shrinks core lowers the
   ceiling to the new measure in the same change, so the cut stays won. Once
   core is below the limit, the ceiling stays at 24,999 and core may use the
   room under it; it never rises above 24,999 again.
5. **No functionality is dropped** (Vision `V-6`). Public APIs, CLI actions,
   the five-port contract, MCP revisions and transports, ACP, A2A,
   OpenAI-compatible and Anthropic behavior, Wasm admission, permissions,
   cancellation, persistence and harness execution keep their behavior. A
   capability leaves core only after its destination works and old and new
   behavior have been shown over the socket. Moving a capability into
   `susi-commercial-apps` this way is not dropping it (CEO decision,
   2026-11-17, asked directly with the trade-off stated: "Moving to commercial
   is fine"): core forwards to the commercial destination, and an installation
   without it names the command that serves it. Implementations move this way
   whether or not they are pillar code (CEO decisions, 2026-11-17): pillar
   implementations, because the GAWD, GEMI and GMCP crates alone were 133,562
   lines ("Move pillar implementations into the paid suite as well"); and the
   feature implementations in the socket crates (daemon handlers, CLI verbs,
   config catalogs), because the other 158,247 lines left core about 58,000
   over the limit with every pillar implementation gone ("Move non-pillar
   features too"). Core keeps the contracts, transport, basic configuration
   and the plane bus. Pillar code runs on Linux before core deletes its copy.
6. **No tricks.** The number never comes from hiding code: not by minifying,
   not by moving runtime code into tests, tooling, `vendor/` or `.agents/`,
   not by leaving compiled code out of the count, and not by deleting tests
   or weakening protocol or security checks.
7. **Only the CEO raises it.** Raising the ceiling, or the limit, needs a CEO
   decision recorded in this document. A measured reason (Mandates 69 and 78)
   no longer lets core grow.

## 2. How an agent keeps it

- Before you add runtime code to core, look for what it can replace
  (Mandate 78), and decide whether it is core at all: a UI, an enterprise or
  department pack, a vendor product integration or a governance plane belongs
  in `susi-commercial-apps` (Mandates 74 and 75).
- Run `cargo test --locked --test lean_core_ceilings -- --nocapture`. It prints
  the complete socket and every package's share, fails when the count is above
  the ceiling, and tells you the value to pin when you have shrunk core.
- `cargo core-size` is the acceptance check for the limit itself. It fails
  until core is below 25,000 lines; then it is green and stays green.
- The ceiling is one constant. When two agents both lower it, resolve the
  conflict by measuring the merged tree and pinning that value.

## 3. What it changes

- **Lean-core program** (`AGENTS.md`, `docs/lean-core.md`): the complete socket
  now has a ceiling that only goes down. The per-pillar, category and per-file
  line ceilings removed by CEO decision on 2026-11-16 stay removed; this is one
  ceiling on the whole socket, set by the CEO the same day.
- **Mandates 69 and 78**: a change that makes core bigger used to need a
  measured reason. It is now not allowed while core is above the limit.
- **Vision `V-6`** stays the reduction program (task `T-CODEXCORE100K-1` and
  its follow-ups). Mandate 84 is the rule that keeps every step it wins.

## 4. Implementation truth

- The native socket retains generic protocol, identity, authorization and
  bootstrap contracts. Concrete command, daemon, agent, model, tool and
  configuration owners run in Susi Commercial Apps. Each further extraction
  must retain original behavior, security boundaries and tested callers.
- `cargo core-size` measures the current complete runtime against 24,999.
  The ordinary size suite enforces `COMPLETE_SOCKET_CEILING`; until the new
  goal is met, each reduction lowers that ceiling to its measured size.
  The counting scope has not changed.
- Main remains development source. Size acceptance is separate from protocol
  conformance and exact-source Windows/Linux production qualification.
- Runtime counts and qualification outcomes belong in local evidence records;
  this document states the current requirement, not a historical measurement.