# Complete native declaration measurement

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
The implementation and tools are frozen pending completion of source reviews.

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
