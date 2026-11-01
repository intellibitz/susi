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
credentials and endpoints removed. It runs arithmetic and prose prompts,
requires successful tool-free mission traces, checks answer semantics, and
requires two audited nonempty Rust Candle generations. The cached session
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
public model download under the default policy. Credentials, cloud endpoints,
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
contract**. Additional Windows versions and architectures require their own
acceptance evidence. Other Windows build targets fail configuration rather
than silently substituting a different runtime.

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
