# Native Rust Susi: delivery contract and evidence

## Step 1 continuation: current gate status

The artifact-level audit found additional issues beyond Cargo package metadata:
the stock Rust standard library's `core/src/unicode` implementation is covered
by Unicode-3.0, and the selected regex-syntax package has a nested Unicode data
license. Production qualification now fails closed without verified standard-
library provenance. Agents are implementing original OS-backed replacements;
these findings do not authorize license exceptions or relabeling copied tables.

The strict Apache-2.0/MIT/BSD-only license check now passes with the local
IDNA and identifier implementations. No allowlist exception was added. They
query installed OS/compiler APIs instead of distributing Unicode tables;
see `unicode-platform.md` and `identifier-platform.md` for their actual
repertoire, capability and resource contracts. Earlier three-blocker results
below remain historical evidence, not the current dependency verdict.

The genuinely uncached Windows installation also passed: native placement,
private startup, authenticated public model/tokenizer download, two actual
Candle answers and scoped shutdown. Production qualification and combined-tree
acceptance remain pending until their final gates run. The CEO's Step 1 of 100
acceptance contract is `step1-acceptance.md`; agents and Susi own its execution.

Windows runtime inspection also found that eliminating redistributable DLL
imports alone did not eliminate copied Microsoft CRT startup implementation.
The original `susi-windows-runtime` Rust crate now supplies executable entry,
TLS metadata and initializer/termination handling. Its qualified compiler is
Rust 1.99.0 at commit `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4` on native
x64 Windows MSVC. Changing that ABI requires fresh qualification, not removal
of the guard. OS UCRT and kernel imports remain explicit platform boundaries.
The final product and developer executables require both PE-import inspection
and artifact-bound link-map evidence; an allowed DLL list alone is insufficient.
Lifecycle fixtures and full Susi inference have passed diagnostic builds;
combined-source release qualification remains a separate gate.

The CEO authorized Hyper-V administration through `gsudo`. Agents have created
an isolated clean Windows 11 VM and verified the OS runtime boundary there.
Its baseline lacks Rust, Git, Python, Node, the C++ compiler and the separate
Visual C++ runtime DLL. The final packaged executable still needs its own
uncached installation and first-goal evidence in that VM before Step 1 closes.

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
  `native-bootstrap.md`. The Rust launcher isolates both Cargo target and
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
An independent native root integration run also passed 35 architecture tests,
five child-profile isolation tests and three source-policy tests. Combined
workspace/all-target Clippy passed with warnings denied. The rebuilt debug
artifact at `200f9d73d` was 95,625,728 bytes; this is not a production-size or
latency guarantee.

Installed-journey testing found issues hidden by unit tests: a directory name
containing `native` incorrectly excluded verified weights, an installed C++
runner took precedence over Candle, and a short prose request launched a swarm
then ended in recovery rather than delivering the requested reply. Agents must
retain these failures and verify the corrected executable with external runners
absent; a successful exit code or an intermediate generated sentence is not
first-goal acceptance.

The corrected artifact at `c83174b5ba57844b561fe66dfeac0773d01eaa01` is
95,548,416 bytes (SHA-256
`C86FE59D997CAF9E4E473A2532A4FABD30E719F519823A4B018E63F60099E16E`).
Its fresh-profile journey used the verified cached Qwen starter, no cloud
credentials, and only Windows system directories on PATH. Both arithmetic and
short prose completed through in-process Candle, without a swarm or external
runner. Total session time was 9.939 seconds; recorded model generation took
1.086 and 2.544 seconds. These are observed debug CPU results on this host,
not a general latency guarantee or proof of an uncached installer journey.
The final inference-package suite passed 419 tests with two ignored, and the
combined workspace/all-target Clippy and formatting checks passed again.
A second fresh-profile run completed both goals in 7.829 seconds. Its foreground
process had an OS-reported peak working set of 1,065,631,744 bytes (1,016.27 MiB)
during model use. Idle memory and reliable opening-prompt latency were not
measured. These baselines are evidence for future budgets, not a zero-bloat
certification. Both runs left no owned processes running after scoped cleanup.

A real three-input embedding comparison against the previous ONNX engine found
minimum cosine similarity 0.999999999999784 and maximum component difference
below 1.3e-7. This is sample evidence, not an exhaustive equivalence claim.

The strict license allowlist remains Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause.
The resolved graph has progressed from 63 rejected entries to three:
`idna_mapping`, `unicode-ident` and `unicode-normalization`, whose expressions
include Unicode-3.0. Older versions with missing notices, license relabeling,
audit exclusions and lossy ASCII substitutes are not acceptable remedies.
Production qualification therefore remains blocked. No compliant production
release, full ten-mandate completion, or uncached clean-machine installation is
claimed. Optional GPU features and Linux platform OpenSSL are not certified by
Windows CPU evidence.

Remaining acceptance includes a fully green qualification on the final combined
source, legitimate Unicode dependency resolution, uncached native installation,
enforced size/idle memory/latency budgets, real Susi release promotion/rollback,
and complete cross-platform Rust-only qualification. Local evidence is retained
under `.artifacts/native-audit/`; task records stay open until their full gates
pass. Agents own this work, including conflicts and repairs.
