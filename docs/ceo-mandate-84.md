# CEO Mandate 84: Susi Core Stays Under 100,000 Lines

Effective 2026-11-16. Binding on every agent in the Susi AI ecosystem — local,
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
Susi core is always under 100,000 lines; until it is, core never grows, so pay
for every line you add to core by removing as many in the same change, lower the
pinned ceiling when you shrink it, put add-ons in `susi-commercial-apps`, and
never drop a capability or hide code to make the number.

## 0. The CEO's words

> make a mandate, susi core should always be under 100k lines.

Recorded as vision `V-9` here and `V-10` in `intellibitz/susi-commercial-apps`. It
continues `V-6`, which the CEO set earlier the same day.

## 1. The rule

1. **The limit.** Susi core's complete socket is always strictly below
   100,000 physical lines: at most 99,999.
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
   core is below the limit, the ceiling stays at 99,999 and core may use the
   room under it; it never rises above 99,999 again.
5. **No functionality is dropped** (Vision `V-6`). Public APIs, CLI actions,
   the five-port contract, MCP revisions and transports, ACP, A2A,
   OpenAI-compatible and Anthropic behavior, Wasm admission, permissions,
   cancellation, persistence and harness execution keep their behavior. A
   capability leaves core only after its destination works and old and new
   behavior have been shown over the socket.
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
  until core is below 100,000 lines; then it is green and stays green.
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

- **Not met today.** At `origin/main` on 2026-11-16 the complete socket is
  307,737 lines (301,283 first-party runtime and 6,454 facades); 207,738 lines
  must still go. Reported beside it: 136,790 test lines, 72,174 tooling lines,
  216,164 lines of third-party `vendor/` source.
- **Enforced:** `repository_size_is_reported_and_unsafe_baselines_do_not_multiply`
  in `tests/lean_core_ceilings.rs` fails when the complete socket exceeds
  `COMPLETE_SOCKET_CEILING`, pinned at 307,737. The suite runs in the ordinary
  test run and in core's fast release pipeline (`xtask/src/fast_release.rs`),
  so a core that grew cannot be released until the growth is paid for. A
  compile-time check keeps the ceiling from being pinned below 99,999.
- **Not enforced:** `main` stays ungated (Mandate 74), so a commit that grows
  core can land there; the next test run and the next release refuse it until
  it is paid for. Nothing checks that a shrinking change lowered the ceiling:
  the test prints the value to pin. "No functionality dropped" and "no tricks"
  are verified by review, regression and conformance runs, not by the count.
- The self-build contract (`crates/susi-core/src/self_build.rs`) carries the
  rule to every delegated coding task in a Susi checkout.
