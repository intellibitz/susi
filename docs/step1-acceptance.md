# Step 1: Susi builds, tests, improves and uses itself

This is the CEO's first step of a 100-step vision. Finish and measure this
step before treating later capabilities as delivered. Agents own the entire
engineering loop; the CEO does not inspect code, configure the environment,
repair installations or perform technical approval. Standing Git commit,
push and merge authority does not waive evidence, credential, spending,
privacy or destructive-action boundaries.

Acceptance requires a real native Windows/MSVC build, an isolated local agent
change, serialized integration of the combined tree, debug and optimized
production artifacts, license/native-source checks, fresh-user operation,
first-goal inference, health verification and a demonstrated rollback. Each
receipt must identify source, dependency lock, toolchain, target, artifact
digest and actual commands/results. GitHub is optional. A fixture proving
the release machinery is not evidence that Susi itself is production-ready.

The production qualifier runs formatting, strict license checks, workspace
Clippy, workspace tests and an optimized native build against the immutable
locally integrated commit. Its packaged gate evidence includes measured CLI
health for version, help and status. Promotion and rollback recheck package
integrity and repeat these checks before changing the atomic current-release
reference. Existing running processes are not replaced or certified by this
reference update.

## Initial resource budgets

Zero bloat means justified capabilities and enforced measured budgets, not
literally zero storage or memory. The initial policy is an executable at most
256 MiB; each CLI health command at most 30 seconds, 2 GiB sampled foreground
resident memory and 2 MiB combined output. These conservative starting ceilings
are informed by the recorded approximately 91 MiB debug executable and 1 GiB
model-use process; they are not performance promises or optimized targets.
Actual values and limits are retained with pass/fail evidence. Changing a
budget requires a reviewed engineering rationale and fresh measurements.

Memory and output are sampled every 10 ms while the owned foreground process
exists. They are detection thresholds, not hard OS allocation limits: brief
spikes and bytes emitted between samples may exceed them. Descendant memory,
model-cache storage and whole-machine use are not included. A timeout or
budget violation kills and reaps the exact owned child and fails qualification.
The evidence must never call sampled RSS an OS peak working set.

## Required inference acceptance and remaining production evidence

Version, help and status do not establish model inference, daemon service
health, an uncached installation or recovery of a running production service.
The production qualifier additionally executes real first-goal prompts using
the exact candidate under isolated runtime state with inherited provider
credentials and endpoints removed. It runs arithmetic and prose prompts as
successive one-shot piped goals sharing one session budget, requires
successful tool-free mission traces, checks answer semantics, and requires
two audited nonempty Rust Candle generations. The cached session
budget is 60 seconds; uncached provisioning and inference have 600 seconds.
Both use the same sampled memory/output ceilings. Actual output, traces,
audits, model identities/digests and measurements are embedded in the
manifest-hashed gate package. An editable external success report cannot
satisfy production acceptance.

Qualification discovers the installed default starter cache automatically.
Agent orchestration may supply a read-only cache with the process-local
`SUSI_QUALIFICATION_MODEL_CACHE` environment variable; users do not configure
this. When all four default model/tokenizer artifacts exist, they are copied
into fresh state and the candidate must admit them before offline inference.
Otherwise the exact candidate performs native install/start and its licensed
public model download through the explicit `susi os manage model install
starter` command (core never downloads a model by itself). Credentials, cloud endpoints,
proxies and third-party tool paths are absent. This explicit uncached phase
needs public network access; Cargo build/test gates remain offline.

`local-release check-inference <candidate>` runs the same check diagnostically
without qualifying or promoting a release. Both modes invoke scoped stop and
check that no process still executes from the private installed runtime.
This is not a claim about unrelated system processes or descendants running
outside that private runtime. Machinery fixtures do not bypass the production
entry point's real inference requirement.

On debug candidate SHA-256
`3ac99979c0a3dcac0192880f57c1d16752d76d9de316e0a9dd5463528103af0b`,
the cached check completed both prompts in 7.382 seconds, sampled
1,036,754,944 bytes foreground RSS and captured 829 bytes output; scoped stop
took 282 ms. An independent uncached check performed native install in 396 ms,
start in 786 ms, public model download plus both goals in 57.865 seconds,
sampled 1,066,831,872 bytes RSS and captured 1,345 bytes output; scoped stop
took 1.316 seconds. Both were actual Windows runs, not optimized-production
qualification. Per-command integration/release gates and local builds now
record elapsed milliseconds; no unmeasured "blazing fast" guarantee follows.

Until integrated acceptance and all strict license gates pass, the
production qualification remains incomplete. Do not label Step 1 complete,
claim every mandate fulfilled, or promote a fixture as the product.

## Platform-adapter and attribution gates

The standalone IDNA and compiler-backed identifier crates have their own
workspaces, so the root workspace test command does not execute their tests.
Integration and qualification now run their library and integration suites explicitly, offline
and locked after generating their private standalone lockfiles. An explicit
local adapter patch prevents accidentally testing the registry implementation.
Missing product adapter manifests fail the gate. Only minimal release-machinery
fixtures without Susi's core crate omit these product-specific checks.

The artifact inventory uses the exact product's selected normal/build graph,
not workspace/dev feature unions from Cargo metadata. Each host/target feature
context is screened independently; a pure feature in one context cannot hide
native compilation in another. Metadata still supplies source identities,
license declarations and attribution files. Missing notice texts are reported
together and block packaging. A permissive SPDX declaration alone does not
supply the upstream attribution text. `local-release check-inventory` audits
this without qualifying or promoting a release.

## Actual promotion and rollback acceptance

No production pointer or qualification reference existed during this review.
After genuine qualification succeeds, agents must promote candidate A, launch
it through `local-release run`, and record its manifest digest, runtime home,
ports and health/inference result. A later independently qualified candidate B
must be promoted and launched through the same command; rollback must restore
A for new launches and repeat health checks. Existing A processes are not
silently replaced. Distinct real artifacts are required to demonstrate a
behavioral version rollback; two receipts for identical executable bytes prove
only pointer movement. Failure/tampering fixture tests remain necessary but
do not substitute for this actual-product evidence. Do not claim running
service replacement or recovery until that lifecycle is separately exercised.

When a published registry package omits its upstream license files, an agent
may preserve the original texts under
`vendor/upstream-notices/<name>-<version>/`. The checked-in provenance must
match the exact Cargo.lock registry checksum, package repository and registry
VCS commit/path. Each file is bound by SHA-256, Git blob SHA-1 and a URL pinned
to that repository/commit/path. Available cached crate archives are also
checksum-verified. Missing VCS/repository metadata requires an additional
pinned source match against actual Rust files in the registry package. Every
path is confined, and empty files or a supplement without a license file fail.
This supplies missing original attribution; it does not override SPDX policy,
relicense code, mutate registry caches or accept Susi's own license as a proxy.

`local-release prepare-next patch` prepares candidate B only in a clean,
freshly claimed `local-agent/` worktree at the exact promoted A source. The
claim must cover all twelve version files. It rechecks A's package integrity
and bounded version output, updates all seven engine manifests and only the
three top-level governance versions plus README badge, then runs an offline
workspace lock update. Non-engine lock entries must remain identical. Original
bytes are snapshotted and synced before mutation; command failures restore all
twelve files and retain the recovery journal. A process interruption leaves
that journal for agent recovery; this is not an atomic multi-file filesystem
transaction. Production references and runtime binary.hash are not changed.
The prepared source still needs a scoped commit, local integration and genuine
qualification before B can be promoted. The htmlescape platform replacement is
also a mandatory standalone regression gate alongside IDNA and identifiers.


### Initial clean-machine access constraint (2026-11-01)

Before explicit Hyper-V elevation authorization, the Windows host,
`FURY16G10-OT54R`, supported successful fresh-profile
installation and inference checks, but these are not clean virtual-machine
proof. A read-only host inspection found the Hyper-V PowerShell commands;
`Get-VM` and `Get-VMHost` both returned “You do not have the required permission
to complete this task” for the current execution token. `WindowsSandbox.exe`
was absent from command discovery. Existing Windows installation images were
found at `E:/opt/install-media/windows/Windows11.iso` and
`E:/opt/install-media/windows/Win11_25H2_English_x64_v2.iso`; their authenticity,
licensing and suitability were not verified, and neither image was mounted or
used. No virtual machine or host feature was changed.

Fresh HOME, empty model caches and an OS-only PATH exercise product bootstrap
without inherited Susi configuration, credentials or developer PATH tools.
They still share this developer host's installed system components. Genuine
clean-Windows-machine acceptance remains unproven until agents can run the
installed production artifact inside an authorized isolated clean Windows
environment and retain its installation, inference and lifecycle evidence.
Local production qualification and rollback work may continue, but Step 1 must
not be declared fully complete on fresh-profile evidence alone.


Windows product qualification also inspects ordinary and delay-loaded PE import
names directly from the exact executable before launching health checks. It
rejects malformed images, unknown DLL dependencies and separate Visual C++
redistributables such as `VCRUNTIME140.dll` and `MSVCP140.dll`; accepted OS import
names are retained inside manifest-hashed gate evidence. This inspection cannot
prove dynamically constructed `LoadLibrary` paths, OS-version availability, or
replace a clean-machine execution test. `release check-pe <executable>` exposes
the same read-only diagnostic. Debug artifact `3AC99979...103AF0B` was observed
to import VCRUNTIME140; optimized production must be inspected separately rather
than assuming its panic/runtime imports match debug.


Stale GMCP source-bound evidence is renewed natively with
`release record-gmcp` on a clean final source commit. The recorder runs the
actual named requirement tests offline, verifies that every selected test ran
and passed, then appends superseding records without deleting historical
records. Commit those records before `release record-gmcp --mastery`; commit
the mastery records afterward. Failed checks or source changes produce no
new records. Both phases reuse this checkout's private `target/local-native`
output and require native MSVC on Windows. Recording is not a skip mode for
integration or production qualification; those gates still execute the full
required suites against the final committed evidence.


### Windows OS-UCRT capability boundary

Native x64 Windows MSVC product linking now generates a COFF **import library**
from an original list of required compiler-runtime symbol names. The generated
metadata directs those calls to Windows' own `ucrtbase.dll`; it contains no
copied CRT implementation, bundles no runtime DLL, and does not enable static
CRT. Native MSVC `lib.exe` is a build tool only. Existing debug unwind and
production abort policies remain unchanged.

The minimum product platform for this boundary is x64 Windows with the OS UCRT
and every compiler-required export present. Actual ABI behavior has currently
been tested on Windows version `10.0.26300` (build 26300) and a clean
Windows 11 Pro `10.0.26200` guest, not on every Windows 10/11 release. This is an explicit tested capability boundary, **not a claim
that these private compiler-runtime exports are a stable Microsoft application
contract**. Additional Windows versions require their own acceptance
evidence.

Release and CI scope (CEO decision 2026-11-03): the public core release and
core CI run on vanilla x86_64 Windows and x86_64 Linux only. ARM64 Windows
(`aarch64-pc-windows-msvc`) portable source support exists: the runtime gate,
OS-UCRT link layer, private sysroot contract and evidence gates accept it on a
native ARM64 host only so certified builds can be produced and qualified
downstream in the `susi-commercial-apps` pipeline. It is not a published core
release, has no core CI leg, and is not production-qualified by core: no ARM64
clean-guest, inference-budget or promotion evidence exists, and the aarch64
`__chkstk` source (`kernelbase.dll`) is a recorded hypothesis until native
ARM64 checks in the commercial pipeline confirm it. Every other Windows build
target, and every cross build, fails configuration rather than silently
substituting a different runtime.

Checked-in native tests generate the same import metadata, then compile and
execute Rust fixtures proving panic payload recovery, destructor execution
while unwinding, nested catches, thread-panic joining, resume-unwind, memory
operations, and optimized abort-profile functionality. A deliberate absent OS
export fails at loader initialization (`STATUS_ENTRYPOINT_NOT_FOUND`) before
application code runs; an intentional abort remains an abort. Both fixture
profiles pass ordinary/delay PE import inspection without VCRUNTIME. These
fixtures are necessary compatibility evidence; final Susi production inference,
its artifact PE gate, and clean-machine acceptance remain separate mandatory
checks. The earlier diagnostic production artifact predates this mapping and
correctly failed the external-runtime gate.


### Clean Windows guest ABI evidence (2026-11-01)

After the CEO explicitly authorized Hyper-V access, bounded elevated helpers
created the uniquely owned `susi-step1-clean-20261101` VM from the existing
Windows 11 25H2 ISO, Windows 11 Pro image index 6. No pre-existing VM was
modified. Later elevated operations use the CEO-requested installed `gsudo`.
The image was applied to a new private VHD; no developer software or Visual C++
redistributable was installed. Secure Boot remains enabled. The VM's virtual
TPM was disabled because this host's virtualization-based security was not
running; no host security setting or reboot was changed for this workaround.

Guest inventory reports Windows 11 Pro `10.0.26200`, no `vcruntime140.dll`, no
Git/Rust/Cargo/Node/Python/MSVC command, and no Susi/model state. The installed
program inventory contains only the image's Edge and WebView2 entries. Windows
setup and OOBE state flags are all zero. PowerShell Direct ran the exact same
hash-verified OS-UCRT ABI fixtures as the host:

- Unwind fixture: `93DAC1BD1D486077BAC7C90BC7F9022F1A84275BA151D28444B8D2E31B186222`.
- Optimized abort fixture: `E1B1275526CE5E8ED0B5A1F3F5E285E4506D2A11D187A1A0A6BA841946F31428`.
- Missing-capability fixture: `D4412EC3E7E24A1FFAE2CC694CB86817D696E4A7B7314BCEC035FA4C9FE14646`.
- OS-linked driver: `B8C78A2D404672DE7CC3BAA5973FC62DFF460559D7655C6DC400DD8CF18A0934`.

Unwind payload/drop/nested/thread/resume and optimized thread/memory checks
passed. Intentional abort returned `0xC0000409`; an unavailable OS export
returned `0xC0000139` before application output. This resolves the earlier
access obstruction and extends observed ABI behavior to a clean second Windows
build. It does **not** attest the final Susi artifact, elimination of compiler
startup objects, or the uncached product installation/inference journey. Those
remain separate acceptance gates.


### Native helper startup and bootstrap

Every executable delivered by the local Rust tooling uses the same original
Rust Windows process boundary: `xtask`, `local-workflow`, `local-release`,
`ci-release-only`, and `susi-dev`, as well as the Susi product. The bootstrap is
now an explicit Cargo binary. Agents perform its initial Cargo build with
process-local target/build directories inside their own checkout, before any
Cargo subprocess can inherit a shared output directory. Bare `rustc` bootstrap
compilation is not the delivery path: it would reintroduce compiler CRT startup
objects. This initial toolchain operation is agent-owned; it adds no user
configuration or mandatory PowerShell workflow. Installed Susi owns subsequent
local build, integration, qualification and improvement commands.

Windows qualification builds the five tooling binaries in release mode and
inspects every helper plus Susi. The PE entry address must equal `susi_pe_entry`
in the actual linker map, backed by a Rust compiler object. The map must not
contain MSVC CRT startup or static runtime implementation libraries/members.
OS import libraries remain metadata, not shipped foreign implementations.
Full map text, map digest, executable digest, entry address and ordinary/delayed
DLL imports are retained in immutable manifest-hashed gate evidence. Production
promotion refuses receipts predating this native-runtime qualification policy
and requires fresh qualification; an older import-only receipt cannot bypass
entry/object checks.

### Linked software license boundary (2026-11-02)

Cargo package license expressions do not prove the licenses of every linked
source file. The selected Rust standard library also belongs to the shipped
software inventory. Production qualification currently fails closed while its
Unicode-3.0 source material is replaced and an audited custom-sysroot provenance
verifier is developed. The diagnostic records the original compiler report,
standard-library copyright text and selected-target archive hashes; an editable
license assertion or absence of Unicode symbol names cannot authorize release.

The private regex-syntax replacement retains upstream parser/HIR MIT/Apache
notices and records removal of the separately licensed generated data in its
SUSI-PATCH.md. Its independent Rust OS-Unicode boundary uses installed OS data;
no license exception, relabeling, bundled ICU implementation or newer-Unicode
compatibility claim is granted. Both the parser and backend standalone suites
are mandatory in integration and production qualification alongside the other
three platform adapters. Missing manifests fail the gate.

### Actual clean Windows product diagnostic (2026-11-02)

The owned Windows 11 Pro 25H2 guest (build26200, 4vCPU, 8GiB RAM), with no
Visual Studio/runtime redistributable/development tools or previous Susi model
cache, ran debug artifact SHA256
`D16396473E2F3E2DC30F15EEA2C6E291B79920BFB7165AD47DEB886253BC925A`.
This was explicitly a license-unqualified diagnostic, not production acceptance.
Native installation and daemon start succeeded; the uncached starter model
finished downloading at approximately45 seconds. The first goal answered
`2+2 equals 4.` and its trace recorded SUCCESS in72 seconds. The second goal,
`Write one short sentence about a red kite.`, failed: model preparation waited
120 seconds, native fallback hit TRUTH_UNVERIFIED, and the trace recorded
DELEGATED after140 seconds with power_reason. The installed stop command then
reported one daemon stopped and ports released. The complete first-use journey
therefore FAILED; neither successful installation nor real Candle token
generation overrides that failed goal.

Raw guest stdout/stderr, mission traces and Candle audit were copied unchanged
into the release worker's ignored `target/clean-vm/guest-journey-failed` evidence
directory. The guest remains preserved for diagnosis; a rerun cannot be called
uncached without a separately verified fresh profile or clean guest. This
result supersedes the earlier host-permission blocker but does not supersede
the still-open standard-library license and production qualification gates.


### Authenticated private standard library

The official Rust1.99.0 rust-src archive was fetched from the Rust distribution
service and its SHA256 verified against the separately fetched official channel
manifest. The tracked hash-only manifest covers all3,608 original library files,
including vendored build dependencies; it redistributes no Unicode tables.
The native verifier admits exactly the reviewed Windows Unicode backend change
and the three documented char const-API removals. Missing, extra, changed or
redirected source files fail verification.

Integration and qualification execute the tracked offline builder freshly for
each acceptance operation. A JSON receipt or Git reference cannot substitute
for that execution. The operation retains an in-memory selection, checks the
stable consumer, and holds Windows read handles denying archive write/delete
through all compiler commands. Artifact hashes are checked again afterward.
All product and local helper builds in that operation receive the same explicit
sysroot after inherited compiler flags are cleared. Original upstream notices
are retained unchanged together with the selected graph and exact source delta.
Production still requires the complete test, source, native-link, inference and
promotion gates; a successful standard-library build alone does not qualify it.

The initial correctness baseline rebuilds the private standard library for each
operation; nested bootstrap/build operations can repeat this work. Build elapsed
time is recorded. Faster cache reuse is intentionally deferred until exact
reviewed artifact pins or equivalent independent verification support it.

### Clean Windows installed journey passed (2026-11-02)

The same owned Windows11 Pro25H2 guest (build26200,4vCPU,8GiB RAM) ran a
separate new local user, SusiCcd038cb7, with an independently recorded absence
of Susi state and Hugging Face/model caches. The previous failed users and their
evidence were preserved. No development tools or runtime redistributable were
installed to make the candidate work.

Exact tested executable SHA256:
`C21753DC041CE2F6C378BBA415A59419B406F4E01C8F9BCEACB7D018B5F307BC`.
Host, guest copy and installed executable hashes matched. This combined debug
candidate included the private standard library, OS Unicode regex backend and
model-residency/provider fixes. It remains a diagnostic candidate, not an
immutable production-qualified release.

| Phase | Measured elapsed |
| --- | ---: |
| Native install | 14.423s |
| Start | 2.621s |
| Uncached model preparation and both goals | 64.701s |
| Stop | 0.761s |

The automatic starter download reached100% at approximately40 seconds. Both
mission traces were SUCCESS with no tools invoked. Arithmetic returned
`2+2 equals 4.`; the requested red-kite sentence completed in9 seconds. Actual
Candle audit recorded8 generated tokens in2,389ms and22 tokens in3,532ms,
plus two warm-up generations. Sampled inference peak RSS was1,039,855,616 bytes.
There were zero owned process survivors and no forced cleanup.

The bounded helper separately recorded foreground exit and output-pipe drain.
The prior B-user60-second observation was below Susi's native120-second start
deadline and is preserved rather than reclassified as success. A separate warm
B-user probe exited in2.187s and drained by2.335s. The fresh C-user run used a
130-second startup observation bound and completed in2.621s; this passed result
does not erase the earlier cold observation.

Evidence remains in the release worker's ignored
`target/clean-vm/guest-product-retry-acceptance.json` and
`target/clean-vm/guest-journey-retry-evidence`, including raw output, traces,
Candle audit, per-phase resource measurements and cleanup results. This closes
the clean-machine installed-journey diagnostic gap for the exact candidate.
Final immutable production qualification, actual A/B promotion/rollback, and
all acceptance checks against the final integrated source still remain required.

## Audited native detached-process boundary

The Windows worktree seeder uses the safe `susi-vendor-winapi::detached::spawn`
wrapper. Its only unsafe implementation is the narrowly annotated function
that calls documented Windows process, string-comparison and handle APIs;
`susi-vendor-winapi` retains `#![deny(unsafe_code)]`. This is an operating-system
ABI boundary, not permission to add unsafe application logic or redistribute
foreign runtime implementations.

The wrapper validates absolute executable and working-directory paths, rejects
embedded NULs, quotes UTF-16 arguments explicitly, and constructs a complete
case-insensitively sorted Unicode environment with duplicate keys rejected.
`CreateProcessW` receives FALSE for handle inheritance and hidden detached
creation flags. It closes both returned handles; the child opens its own log.
The caller removes all three Cargo output-directory variables without mutating
the parent environment. No shell is involved in this Windows launch.

Acceptance must retain actual Unicode/quote argument round trips, pipe EOF
while a detached child remains alive, and invalid-input/missing-executable
regressions. Changes to this boundary require independent review and measured
unsafe-exposure evidence; the existing baseline must not be reset merely to
make a gate pass. Launch success proves process creation only, not completion
of background seeding or production qualification.

## Combined verification and remaining release qualification (2026-11-02)

Attempt `step1-acceptance-v6-1793564732671614900-21952` passed all 14
mandatory commands: formatting, strict licensing, workspace Clippy, all
workspace tests and doctests, and all five replacement-adapter suites. It
rebuilt and authenticated the private standard library before compilation.
Clippy took 143.643 seconds; workspace tests and doctests took 931.488 seconds.
Publication nevertheless failed closed because adapter lock generation added
an unused local-patch stanza to the tracked regex lockfile. The candidate and
failure remain preserved; passing commands did not publish an accepted source.
The deterministic four-line lock metadata is now tracked, and two repetitions
of the exact offline generation command produced identical lockfile bytes.

The native seeding regression passed 21 tests with zero failures and one
existing ignored legacy case in 50.15 seconds. A separate detached-worker test
passed 20 consecutive Git invocations. Windows host-tool probes now suppress
console creation, and the shared bounded-command regression checks actual
window visibility, captured output, stdin closure and timeout cleanup. This
addresses the orphaned terminal error windows observed during verification;
it does not claim external llama.cpp inference was used. The new hidden-probe
source change still requires combined immutable integration.

All 60 Windows CI contract tests also passed with PATH restricted to Windows
system directories. Unix-hosted script behavior remains tested on Unix;
native Windows matrix assertions parse the tracked data directly in Rust.

Step 1 remains incomplete until the final combined source publishes an
acceptance receipt, the optimized product qualifies, that exact production
artifact passes the clean Windows installed journey, and genuine A/B
promotion, runtime health and rollback are demonstrated. GitHub publication
or a quota-driven source merge does not waive these production gates.

## Production release C: clean 8 GiB journey, promotion and rollback (2026-11-02)

Release B (0.21.39, release `7b7ad72a…`, source `423cf1c0`) qualified but
failed the clean 8 GiB Hyper-V journey and was never promoted. Its exact-memory
admission worked: the fresh user's starter download completed in about 85
seconds. The first goal still timed out: the shell's 60-second model scan
cache, invalidated only by in-process downloads, hid the model the daemon had
published until after the 120-second wait expired. The ladder's recovery
attempt then rejected an uncited arithmetic answer under the unchanged
truth-governance rule. Evidence is retained as
`guest-journey-production-b-task-evidence`.

Release C (0.21.40, task T-STEP1C-1) keys the scan cache on the shared models
directory's modification time as well. A regression test fails on the old key
and passes on the new one.

| Gate | Result | Wall clock |
| --- | --- | --- |
| Combined integration, 14 commands | accepted `f902d660`, receipt for `b9e3410f` | 1076 s |
| Production qualification | release `b8bb0f99…`, artifact SHA256 `0f48535a…4b7a` | 1585 s |
| Clean 8 GiB VM, fresh user `SusiBTeff6e655`, uncached | passed; both goals answered by in-process Candle; zero forced cleanup | 138 s |
| Promote C | current `b8bb0f99…`, previous A `a3687f7f…`; `run --version` 0.21.40 | 3.4 s |
| Roll back to A | current A; `run --version` 0.21.38 | 2.4 s |
| Re-promote C | current C, previous A; `run --version` 0.21.40 | 2.4 s |

In the journey the starter downloaded in about 40 seconds, the first goal
continued as soon as it landed, and the whole inference phase took 67 seconds
with 1.06 GB peak sampled memory. Logs are in
`.artifacts/native-audit/step1-c-integration.log`,
`production-c-qualification.log` and `production-c-promote-rollback.log`.

Both limits found here are closed by release D below.

## Production release D: slow downloads and tool-free recovery answers (2026-11-02)

Release D (0.21.41, task T-STEP1D-1) closes both limits left open by C.
Downloads keep the fixed 60-second total per transfer call, but only calls
that add no resumable bytes spend the four-attempt budget, and progress
refunds it. A download needing five cut-short calls now completes under a
two-stall budget. In recovery, a local in-process rung on a mission that
captured nothing to cite self-cites its inference receipt. Its answer then
carries the same "No citations found" warning as the first attempt. Captured
evidence must still be cited, cloud prose stays strict, and the strict gate
itself is unchanged.

| Gate | Result | Wall clock |
| --- | --- | --- |
| Combined integration, 14 commands | accepted `083e9d73`, receipt for `871a605d` | 1101 s |
| Production qualification | release `7d343072…`, artifact SHA256 `50571b2b…8683` | 1466 s |
| Clean 8 GiB VM, fresh user `SusiBTf3485fea`, uncached | passed; download about 40 s; both goals answered; zero forced cleanup | 139 s |
| Promote D | current `7d343072…`; `run --version` 0.21.41 | 3.5 s |
| Roll back to C | current C `b8bb0f99…`; `run --version` 0.21.40 | 2.5 s |
| Re-promote D | current D, previous C; `run --version` 0.21.41 | 2.7 s |

The VM journey did not reach the recovery ladder, so the recovery change is
proven by unit tests, not by the journey. The first VM attempt did not run:
the three-hour administrator cache had expired and the UAC prompts were
unattended. It ran after fresh approval.

### Releases E and F on Windows

Release E (0.21.42) established verified installation through the Start Menu and ordinary PATH, including rollback to D and restoration of E. A standalone Windows task subsequently found that the Codex-hosted installer had written to a virtualized filesystem. Agents repaired the actual user installation and repeated the launch and rollback checks outside Codex. [Release E standalone evidence](../.agents/evidence/release42-standalone.json)

Release F (0.21.43) is the current qualified Windows production release, with E retained for rollback. On the clean 8 GiB Windows guest, installation start to prompt was 8.718 seconds. The first correct local answer arrived at 56.899 seconds and the second at 58.875 seconds. The 44.301 seconds between the model download notice and the completion notice include transport, validation and publication. A limited Windows task verified PATH and Start Menu launches through F, E and restored F. All six pre-existing production processes survived. On the separate cache-seeded host, the first answer took 4.444 seconds and the repeated goal took 2.079 seconds. The cold first-answer speed target and byte-identical standard-library rebuilds remain open. [Release F production evidence](../.agents/evidence/release43-windows.json)

## Change to live on the dev channel under 60 seconds (2026-11-06)

`cargo run -p xtask --bin local-channel -- deliver [--prove-rollback] [-- <susi args>]`
(task T-STEP1LOOP-1) takes the worktree's committed `HEAD` to the live dev
channel in one command and times every phase:

1. **scope**: refuse an unclean tree; diff `HEAD` against the dev live lineage
   ref `refs/susi/dev-live` (or its merge base with `origin/main` on first use);
   refuse a `HEAD` that does not contain that ref, and refuse inputs that can
   change every package (lockfile, toolchain, Cargo configuration, `vendor/`),
   which belong to the full gate. The packages that own the changed files are
   selected with the same ownership rule as the integration gate.
2. **test**: each owning package's library and binary unit tests, with
   credentials, `SUSI_*` settings and user homes removed.
3. **build / stage / startup / goal**: the dev executable, its digest-named
   slot, `--version`, and the governed first goal (a confined native read of a
   fresh nonce file under `local_only`, offline; no model inference), timed to
   first output and completion.
4. **merge**: compare-and-swap fast-forward of `refs/susi/dev-live` to `HEAD`.
5. **promote / observe**: atomic `channel.json` update, then the promoted
   executable runs the observation arguments and its output is recorded.
6. **rollback / restore** (`--prove-rollback`): roll back, observe the previous
   build, re-check the delivered digest and re-promote it.

A failed test, build or check promotes nothing. Evidence for each delivery is
`<channel>/deliveries/<stamp>-<commit>/delivery.json` plus a line in
`deliveries.jsonl`.

Measured on this host (Intel i7-13850HX, 28 logical CPUs, 224 GB, Windows
x86_64, other agents active), warm worktree target, each a real commit:

| Change | Packages tested | test | build | goal | change to live | rollback + restore |
| --- | --- | --- | --- | --- | --- | --- |
| CLI about text + help snapshot (root package), rep 1 | `susi` | 10.4 s | 7.9 s | 1.6 s | **21.0 s** | 0.5 s |
| same, rep 2 | `susi` | 10.7 s | 8.5 s | 1.8 s | **22.2 s** | 0.5 s |
| same, rep 3 | `susi` | 12.2 s | 8.8 s | 1.9 s | **24.3 s** | 0.6 s |
| new public constant in `susi-paths` (deepest crate), rep 1 | `susi-paths` | 2.5 s | 48.4 s | 1.6 s | **54.2 s** | 0.5 s |
| same, rep 2 | `susi-paths` | 2.2 s | 41.5 s | 3.9 s | **49.4 s** | 1.2 s |
| same, rep 3 | `susi-paths` | 3.9 s | 47.0 s | 3.0 s | **57.0 s** | 0.9 s |

For the root-package changes the promoted executable's `--help` showed the new
text, the rollback showed the previous text and the restore showed the new text
again (`behavior_differs: true`). The `susi-paths` constant is not observable
from the CLI; those rows measure the worst-case rebuild (25 crates recompiled),
and their observation is `--version` only.

Failures seen on the way: the first probe changed the about text without its
help snapshot, the root unit tests failed (`cli_help_snapshot.txt` drift) and
nothing was promoted. Two warm-up deliveries took 212 s and 126 s: their diffs
included the tool itself (all `xtask` binary tests) and other agents' shipped
changes, compiling those test targets for the first time.

### Linux x86_64, tests concurrent with the build (T-CLAUDELOOP10-1, 2026-11-11)

`deliver` now compiles the owning packages' unit tests in their own warm
target directory (`<target>/deliver-tests`), runs them as soon as they are
compiled and builds the dev executable at the same time. Before, the test
compile, the test run and the build ran one after another, two of them on
one Cargo build lock. Nothing is staged until both have finished and every
test has passed; a failure prints the failing log's tail and promotes
nothing. Measured in a 4-vCPU cloud container (Rust 1.99.0, warm registry),
each a real commit with `--prove-rollback`:

| Change | sequential (before) | concurrent (now) |
| --- | --- | --- |
| CLI help text + snapshot (root package) | 8.5, 7.3, 6.9 s | **5.1, 5.0 s** |
| function body in `susi-core` | 21.1, 16.6, 16.1 s | **9.6, 9.6 s** |

Cold build of the `susi` binary into an empty target: 60.2 s. Warm edit to
binary: 2.4-7.8 s by crate. The first delivery after this change pays a
one-time 94.6 s and 2.4 GB to fill `deliver-tests`; a delivery whose diff spans
root and core runs both suites (18.4-18.6 s). A stale snapshot in the root
package was refused with the channel digest and `refs/susi/dev-live`
unchanged. Evidence: `.agents/evidence/loop10-T-CLAUDELOOP10-1.json`.

What this does and does not establish:

- The Mandate 62 criterion, a genuine source change reaching the verified live
  executable in under 60 seconds, holds for the **dev channel** on this host
  for both measured change shapes. The worst case left 3 s of margin; a busier
  host or a larger change can exceed the budget, and the record says so
  (`within_budget`).
- Tests of packages that depend on the changed one are not run by this loop.
  They run in the integration gate (`cargo local-workflow ship`, about 19 min
  for core), which still precedes `origin/main`.
- The **production** channel is unchanged: an optimized executable still needs
  `cargo local-release qualify` (about 25 min of gates in prior evidence) before
  promotion. This loop does not make a production release live in 60 s.
- Step 1's other open items stay open: millisecond user-response budgets, the
  cold first-answer speed target and byte-identical standard-library rebuilds.

**CEO decision 2026-11-06 ("Dev copy counts").** The dev channel is the
"verified live executable" for Step 1's 60-second criterion. The criterion is
met by `local-channel deliver` with the measurements above, and agents keep it
met: a delivery whose record shows `within_budget: false` is a regression to
fix, not an accepted result. The decision does not change production: an
installed release still goes live only through `cargo local-release qualify`
and promotion, and the integration gate still precedes `origin/main`. Step 1
itself stays open on its other items (millisecond user-response budgets, the
cold first-answer speed target, byte-identical standard-library rebuilds).

## User-response budgets (2026-11-06, T-RTBUDGET-1)

`startup-gate` (schema `susi-startup-gate-v2`) now judges absolute millisecond
budgets on the optimized executable that ships, daemon down and up:

| Surface | Budget | Rationale |
| --- | --- | --- |
| `susi --version`, `susi --help` | 30 ms | 16–18 ms measured on a quiet host; 24–28 ms under other agents' load |
| `susi status` | 50 ms | unchanged; 19–28 ms measured |
| piped first goal, first output | 600 ms | the goal's acknowledgment: nothing is printed before the answer |
| piped first goal, completion | 750 ms | |

The goal is a tool-free, offline read of a nonce file under `local_only`, with
provider credentials removed; every run must answer with the nonce.
Model-backed first answers are outside these budgets (the cold first-answer
item below). The dev channel executable is not judged. The cost of starting a
do-nothing program (the floor) is reported as host-load context only: it moved
between 6.7 and 25 ms on this host while other agents ran gates, and a
multiple of it passed and failed the same binary on consecutive runs, so it no
longer sets a budget.

Causes found and fixed: a first goal spent 0.83–1.8 s registering tools,
because each of about 400 plane-bus rendezvous and capability endpoint files
was flushed to disk on every start (they now use an atomic replace without a
flush: they are per-process metadata that a crash makes stale anyway). While
the daemon ran, its readiness scan also launched one `icacls` per bus file
whose name contains "key" or "token", which slowed goals and surfaced launch
errors to the user; the scan now skips `bus/<pid>/{topics,prefixes,caps}` and
still checks the bus key and real key files.

Measured medians (9 runs each, optimized executables, floor recorded):

| Executable | Floor | version | help | status down/up | goal first output down/up | goal completion down/up |
| --- | --- | --- | --- | --- | --- | --- |
| published v0.21.51 (before) | 11.8 ms | 23.6 | 28.0 | 27.3 / 39.0 | 1050 / 1190 | 1056 / 1197 |
| published v0.21.51 (before) | 13.7 ms | 26.4 | 24.2 | 22.6 / 41.4 | 1171 / 1551 | 1179 / 1564 |
| flush fix only | 8.1 ms | 17.5 | 17.9 | 20.1 / 22.0 | 254 / 348 | 260 / 356 |
| flush fix only | 8.1 ms | 17.6 | 17.6 | 19.0 / 21.9 | 314 / 468 | 321 / 475 |
| flush fix only | 9.8 ms | 27.5 | 23.8 | 24.1 / 27.9 | 297 / 335 | 303 / 342 |
| both fixes, interleaved with before under load | 10.7 ms | 20.5 | 24.0 | 23.5 / 26.5 | 407 / 630 | 415 / 638 |
| both fixes, interleaved with before under load | 19.6 ms | 34.0 | 32.4 | 32.0 / 31.6 | 415 / 754 | 423 / 765 |

Before the fixes every goal case missed its budget by about 2x. With the fixes
and a floor of 8–10 ms, every case was within budget. The both-fixes runs were
taken while other agents' whole-workspace gates loaded this host (floor
10.7–19.6 ms, against about 8 ms quiet): the daemon-down goal stayed within
budget and the daemon-up goal did not. A third both-fixes run at a 15.6 ms
floor took 1.2–1.3 s for every goal case and is not counted. A quiet-host
confirmation of the both-fixes build has not been taken: the host was never
quiet for 100 minutes on 2026-11-06, and the CEO then directed no further
tests. Until it is, these budgets are declared and met on a quiet host for the
flush-fix build only.

2026-11-12 (`T-CARBONCLOUD-2`): `IpcPlaneBus` and `IpcCapabilityRegistry` now
consolidate per-process endpoint and capability metadata under `<cache>/bus/<pid>/`
into single atomic manifests (`routes.json` and `caps.json`) with batched
`ToolRegistry::global()` bootstrap (`CapabilityRegistry::with_batch`), while
retaining legacy `topics/`, `prefixes/` and `caps/` fallback discovery and
dead-endpoint pruning. Measured on Linux `x86_64` (`fast-release`, `startup-gate --runs 9`,
floor `0.75 ms` before / `0.70 ms` after): per-process `<SUSI_HOME>/bus/<pid>/`
file count dropped `440 -> 3` (`-99.3%`), `goal_first_output` down/up improved
`71.67 / 80.79 ms -> 66.09 / 72.57 ms`, and `goal_completion` down/up improved
`73.03 / 82.63 ms -> 67.56 / 73.92 ms`.

Open: a quiet-host Windows `x86_64` confirmation of the consolidated
`routes.json`/`caps.json` build has not yet been recorded on the Windows host.
Production qualification does not yet run `startup-gate` or the 2000 ms
host-contract conformance budget, so a release that misses them is not
refused.
