# Native Rust Susi: delivery contract and evidence

The CEO does not inspect code. Susi is built and delivered by agents. Agents have
standing authority to commit, push, resolve conflicts and merge verified work;
paid services, destructive actions and unrelated publication still require their
own authorization. The governing requirements live in `.agents/identity.json`
and `AGENTS.md`. Other agent entrypoints defer to that contract.

## Product proposition

**Susi: AI that earns its next upgrade.**

A local-and-cloud AI operating environment promotes improvements only with
measured task outcomes, preserved evidence and a verified rollback path. This is
the intended differentiator, not a claim of market uniqueness or completed
recursive self-improvement. Existing receipt/training mechanisms and the new
release evidence are concrete building blocks. The harness memory placeholder
still produces vectors without storing/retrieving records and must not be
marketed as durable memory.

## Current implementation

- Native development starts through `xtask/bootstrap/susi-dev.rs`; see
  `native-bootstrap.md`. The std-only launcher isolates both Cargo target and
  intermediate build directories before compiling any tooling. Windows selects
  MSVC. No Bash, WSL, Python, Node or GitHub service is required for this path.
- `workflow start/commit/integrate/release` reserves private worktrees and scopes,
  checks actual committed edits, builds a merged candidate, runs local gates and
  atomically publishes its integration reference and receipt. Reservations are
  cooperative, not an OS filesystem security boundary. Cross-host coordination
  still requires a shared coordinator.
- Debug and optimized candidate builds are separate. `release qualify` builds
  an immutable snapshot of the verified integration ref, packages executable,
  hashes, dependency notices, SBOM and native-source screening evidence, and
  smoke-tests it. `promote`, `rollback`, `status` and `run` use verified package
  references. Failed gates never authorize a production artifact.
- Worktree runtime homes and cooperative port allocations are private. Local
  verified releases have persistent state and their own port allocation; they
  do not replace or restart the separately installed production daemon.
- Embeddings use Candle BERT with pinned MIT BGE weights, 384-dimensional CLS
  vectors and normalization. Tokenizers uses Rust fancy-regex and suffix arrays.
  The prior ONNX Runtime and Oniguruma paths are removed from the default graph.
- Wasmi replaces the Wasmtime/Cranelift stack. Its bounded WASI host preserves
  tested arguments, streams, clocks, randomness, exit and denied host access.
  See the sandbox facade README for fuel, memory, output and compatibility limits.
- Windows TLS uses Schannel and platform trust, including the MCP transport.
  Certificate/hostname failures are rejected, and remote identity failure cannot
  silently select plaintext. See `vendor/native-tls/SUSI-PATCHES.md` for importer,
  key-lifecycle, older-identity and non-Windows platform limitations.
- Local patches replace disallowed default hash/compression dependencies and
  self-referential tensor ownership. Original notices remain intact. Their exact
  provenance and modifications are recorded under `vendor/`.
- Native model admission verifies immutable publisher revisions, allowed license
  declarations and declared lineage, and artifact hashes. Editable sidecars cannot
  establish trust. The audited starter and BGE artifacts have pinned offline proof;
  other cold loads require authenticated publisher access. Unknown local bytes are
  preserved. Unverified managed Ollama pulls are refused. See the model vendor's
  MODEL-LICENSE.md for exact scope and remaining limitations.
- `.claude`, `.codex`, CLAUDE/GEMINI/Copilot and runtime delegation directives
  follow the CEO contract. Obsolete automatic shell startup hooks were removed.

## Verified evidence and open gates

The source baseline is version 0.21.38 at local commit
`3eb12285cb06294d8c3b765c84757940667e7bdf`. The primary checkout and its existing
evidence edits are preserved. Development occurs in isolated worktrees.

Component checks have passed: native MSVC debug compilation; 14 integration
workflow tests; eight release workflow tests; three bootstrap tests; 19 xtask
unit tests; 16 Candle tests; 26 sandbox tests; five runtime-isolation tests;
owned-tensor movement/source-drop validation; and a 2,500-term compressed index
write/reopen test. The sandbox also executed a compiled Rust WASI fixture.
Windows HTTPS tests reject untrusted, expired, mismatched and wrong-host identities.
These results refer to their tested source snapshots, not every later edit.

The broad Windows workspace library run exercised 85 packages: 3,452 tests
passed, eight failed and 15 were ignored. Agents repaired the eight failures
(native command fixtures, obsolete mandate assertions, inherited cloud endpoint
settings and a task/receipt timestamp race). The five affected packages then
passed 1,136 tests with five explicitly ignored. Nested deliberately failing
test fixtures are not product failures. The updated integration/release tooling
also passed its 22 tests. This is a broad run plus focused repair verification,
not a claim that every acceptance gate passed on one final source snapshot.

A real three-input embedding comparison against the previous ONNX engine found
minimum cosine similarity 0.999999999999784 and maximum component difference
below 1.3e-7. This is sample evidence, not an exhaustive equivalence claim.

The strict license allowlist remains Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause.
The resolved graph has progressed from 63 rejected entries to three:
`idna_mapping`, `unicode-ident` and `unicode-normalization`, whose expressions
include Unicode-3.0. Older versions with missing notices, license relabeling,
audit exclusions and lossy ASCII substitutes are not acceptable remedies.
Production qualification therefore remains blocked. No compliant production
release, full ten-mandate completion, or clean-machine first-goal acceptance is
claimed. Optional GPU features and Linux platform OpenSSL are not certified by
Windows CPU evidence.

Remaining acceptance includes the final combined regression suite, legitimate
Unicode dependency resolution, native installation/first-goal verification,
measured size/idle memory/latency budgets, real Susi release promotion/rollback,
and complete cross-platform Rust-only qualification. Local evidence is retained
under `.artifacts/native-audit/`; task records stay open until their full gates
pass. Agents own this work, including conflicts and repairs.
