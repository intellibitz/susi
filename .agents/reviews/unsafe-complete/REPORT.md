# Complete native declaration measurement

**Final outcome:** reviewed v2 migration and live source admission passed on
clean combined commit `7cfb5df2829ede4312c34332a7c1971d9b62f250`. The final
source-bound review covers 77 components, 320 dependency files and six exact
first-party NN guards. The original baseline and every earlier rejection are
preserved below. Full native integration and production release qualification
remain separate parent-owned gates.

The native scanner replaces partial cargo-geiger parsing with complete,
source-bound measurement of the existing **crate-forbid declaration proxy**.
It does not count unsafe functions or certify the full Rust item grammar.
The actual `signal-hook-registry 1.4.8` bare `Fn` alias tokenizes unchanged;
the file is measured as lacking an unconditional `forbid(unsafe_code)`.
No cached registry source was edited.

The scanner obtains the actual root package's host normal-dependency tree
from locked offline Cargo, and binds its full output, arguments and Cargo
version. Cargo metadata supplies unambiguous full package identities and
declared entrypoint paths. Metadata alone is insufficient because workspace
feature unification can include optional dependencies absent from the root
build. Unsupported or ambiguous Cargo displays fail closed. Explicit
workspace membership defines first-party status; arbitrary path packages
and names beginning with `susi-` receive no exemption.

All selected lib/bin/proc-macro entrypoints must exist, stay inside their
package, tokenize, and retain their hashes through scan completion. Missing,
duplicate, partial, malformed or mixed-context measurements cannot produce a
clean receipt. Tokenized crate attributes distinguish unconditional forbid
from deny, conditional attributes, item attributes, strings and macro text.
The source checkout must be clean and committed before and after measurement.

The comparator binds exact package identities, entrypoint hashes, selected
features and normal dependencies. New feature/dependency selections require
review; removing capabilities is allowed. A baseline migration requires the
same scanner and Cargo context on two distinct clean revisions, the exact
historical inventory, and source-bound component reviews. Carry-forward
requires unchanged measured source, no selection expansion, and an exact
historical name/version/provenance entry. The baseline retains the matched
scan proof and review; it cannot declare a carry-forward basis without that
proof. The legacy baseline projection is checked against the complete proof.

Measurement JSON is unsigned consistency evidence, not standalone
authentication. Production admission must independently invoke the pinned
scanner on its actual detached source, capture the exact output, and verify
source/tree/lock/tool/output digests. The production gate owns that trust
boundary. Builds, advisory/provider gates and actual inference remain
separate requirements.

## Actual verification

`build-proof.json` binds implementation inputs, tool binaries, exact scan
outputs, historical inventory and upstream contract sources. The private
development source copy contains base `cbf85824` plus this worker's two
first-party dependency edges. It is not falsely presented as the final
combined release source. Final combined locked acceptance is still required.

* Scoped tests: **54 passed, 0 failed, 0 ignored**, exit 0, **3.573505 s**.
* Scoped Clippy, all targets with warnings denied: exit 0, **1.491841 s**.
* Clean `aff638d06825f0874231ff01b546f9128a534d04`: **483 packages,
  504 files**, exit 0, **3.866181 s**.
* Clean `cbf858241279f2ce22fdac95baf94bc22d2c4f9e`: **479 packages,
  500 files**, exit 0, **3.747706 s**.

Both scans used scanner SHA-256
`625bdb66edd3978309da28cd6494143b45cdbe815a51debf238467b7533f6386`.
Comparator SHA-256:
`48d969ebd85e62ed94ee4ce143ab5f89530673d0dc379f18e3a4113fb129c725`.
That initial implementation/tool pair is retained as historical evidence; the recursive-JSON follow-up below supersedes its tool pins.

## Historical scope and migration status

The old cargo-geiger 0.13 defaults already requested normal dependencies and
the compiler host. Its entrypoint mode includes lib/bin roots and excludes
custom build scripts, tests, examples and benches. Exact upstream files and
hashes are in `build-proof.json`. This repair preserves that declaration
metric while selecting the actual root build graph and refusing partial scans.

The old run reported 490 measured packages and omitted signal-hook-registry.
Its graph also included 12 packages outside the actual selected root graph:
bit-set 0.11.1, bit-vec 0.10.1, proptest 1.12.0, quick-error 1.2.3,
rand_xorshift 0.5.0, rusty-fork 0.3.1, unarray 0.1.4, wait-timeout 0.2.1,
objc2 0.6.5, objc2-core-foundation 0.3.2, objc2-encode 4.1.0 and
objc2-foundation 0.3.2. `old-geiger-root-selection-delta.json` records each
exact old identity and metadata reverse edge. The proptest closure belongs
to workspace/test feature selection; the Objective-C closure is absent from
the actual Linux root selection. No selected production component is waived.

The first migration attempt correctly exited **2 in 2.985990 s**: the exact
historical identity check requires **77 component reviews**, while only 24
were complete at that point. The remaining 53 version/provenance changes had
been hidden by the old name-only ratchet. `migration-required.json` records
the complete set; `migration-run.json` preserves the exact rejection.
`historical-v1.json` preserves the original baseline, whose SHA-256 is
`9d8fd1fa64ec943f6978bff0a9225ae7ba1f0e9098a8672a528dd982cfc3e623`.
The baseline remains unchanged until all required source reviews are bound
and the native migration passes. No release qualification or promotion is
claimed by these receipts. Private model data and build outputs stay outside
this source review directory.

## Recursive JSON follow-up

The native decoder now rejects duplicate object keys recursively, including
dynamic Cargo metadata and embedded historical inventory JSON. Typed structs
alone could reject their own duplicated fields while losing duplicates in
`serde_json::Value`. The regression covers nested metadata keys and historical
proof keys. No graph or review policy was relaxed.

The final v2 pair, preserved separately under `pinned-tools-v2`, is:
* Scanner: `e870c551ae413c96188fb848275b26e41f1673324cfd93b4f5c8549a35c0fc88`.
* Comparator: `6f9c5298d6fc1163da1284a4ebfeea6b86d0848288fbdcab3c30aef6aed40956`.

`build-proof-v2.json` records implementation file hashes and refreshed actual
scans: aff638d0 483 packages/504 files, exit 0, 3.354196 s; cbf85824 479/500,
exit 0, 3.216655 s. Scoped tests passed **55/55** in **3.857478 s**; Clippy
all targets with warnings denied passed in **0.874643 s**. Prior receipts
remain unchanged. Final migration still needs a fresh clean post-repair
candidate and all accepted source-bound component reviews; discovered candle
boundary findings must be treated before that component can be admitted.

## Final reviewed-file and source-guard admission

Component reviews can cover nested modules outside the crate entrypoint.
Admission now rechecks **every reviewed dependency file** against the actual
selected package. An unchanged lib.rs cannot carry approval across a changed
safetensors implementation. A missing file or symlink escaping its package
fails closed. Each component also declares `source_guards` (an explicit empty
list if none): source-root-relative paths/digests binding first-party export
and dispatch restrictions on which scoped retention relies. Migration and
admission verify these guards; changed/missing facade guards require renewed
review even when dependency entrypoints/features are unchanged.

The final immutable pair is under `pinned-tools-v3`:
* Scanner: `b703e02c66a4d47b49d2cf2d533fd4628aa6d90bfd4379dcb35e6a0287d9ed0e`.
* Comparator: `6b336484a60da89d926ea2e6816dd9940d609d47b44a0cb4c563a21a9894092e`.

`build-proof-v3.json` binds this implementation and tool pair. Scoped tests
passed **56/56 in 2.349698 s**; Clippy all targets with warnings denied passed
in **0.436793 s**. Changed/missing nested source, matching-byte symlink escape
and widened/missing first-party guard regressions all executed.

All 77 independent source review records now match the required identity set;
319 reviewed file bindings were checked against retained historical cbf
source. The real native comparator rejected the two explicit candle holds
with exit 2 in 5.467451 s, creating no output and preserving v1 baseline bytes.
Those historical reviews do not approve the discovered defects. Final
migration requires the repaired clean combined candidate, updated core
review, guarded NN retention and fresh exact measurement bindings.

## Final migration and live admission

The final clean combined source is
`7cfb5df2829ede4312c34332a7c1971d9b62f250`, tree
`94e5516b6961994131089e9dc3170bf6c2d0e6a8`. Final v3 scanner measured
479 packages/500 entrypoint files, exit 0 in 2.748914 s. Same-tool base
`aff638d06825f0874231ff01b546f9128a534d04` measured 483/504, exit 0
in 2.935134 s. Exact inputs and tool pins are preserved in the baseline proof.

The native migration passed with exit 0 in 6.473733 s. A separate live
admission passed with exit 0 in 5.914207 s and no unreviewed exposure.
All 77 component decisions include concrete boundaries and residual risk,
with 320 dependency-file hashes and six source-root guard hashes checked.
The repaired core includes safe numeric byte conversions and explicit F8
dispatch; the NN defect remains upstream but is unavailable through the exact
reviewed first-party facade/dispatch routes. The optional wasmparser
component-model defect remains excluded by the enforced feature context.
These are scoped decisions for the bound source, never all-API assurances.

`final-migration-proof.json` and `candidate-final-ratchet-receipt.json` bind
the actual baseline, tool, source, lock, graph, measurement and review
digests. `review-sources/` preserves the independent records and final
combined Candle supplements without rewriting their historical conclusions.
No product code changed after the tested c3877f873 implementation; this final
commit contains baseline/review/task documentation only. Native full combined
acceptance and immutable production gates must still run before promotion.

## Combined architecture qualification repair

Native A-04 reached real tests and failed the architecture rank check:
`susi-unsafe-ratchet` still declared rank 0 despite depending on the rank-0
parser implementation. It now declares the minimal valid rank 1; the global
strictly-downward comparison is unchanged and neither crate receives an
exception. The existing layer-matrix test additionally restricts the scanner
to its parser/serialization/hash dependencies, preventing rank-0 HTTP/SDK or
arbitrary pure-facade imports. ARCHITECTURE.md documents the actual boundary
and GAWD's dev-only caller. The claim/task scopes were extended natively from
a clean worker before changing the architecture test and document.

All **36 existing architecture tests passed**, exit 0 in **0.543043 s**,
against actual combined A-04 source `d88ae7939eba14e963e610918e4115ac3274352a`
using the owned changed test source. Pinned rustc 1.99.0, edition 2021,
CARGO_MANIFEST_DIR and the audited native sysroot are recorded with actual
core/serde facade library and host derive input hashes. Compilation exited 0
in **0.805288 s**. An initial mistaken attempt without those libraries failed
compilation and executed zero tests; no missing dependency was stubbed or test
removed. A private manifest counterexample adds HTTP at rank 0: the unchanged
downward rule passes, while the new offline-tool boundary correctly rejects
it. The real failed candidate was never edited.

`cargo fmt --all -- --check` passed in **3.559070 s**. Product code, unsafe
baseline and pinned v3 binaries remain unchanged. Full native combined
integration is still required; this focused repair does not claim release
qualification. Detailed receipts are under `architecture-repair/`.
