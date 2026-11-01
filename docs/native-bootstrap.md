# Native development bootstrap

Installed Susi exposes the same launcher through `susi local build`,
`susi local workflow` and `susi local release`. This dispatch runs before model,
daemon or runtime initialization, so development does not require inference or
cloud credentials. Agents use the standalone Rust launcher for the initial build
before an installed Susi exists. Both entry points share the same Rust source.
It sets
private Cargo output before Cargo compiles xtask, so an inherited shared target
directory cannot contaminate even the launcher build. No Bash, PowerShell script,
Python, Node, GitHub service, or user-written configuration is involved.

For the first bootstrap, an agent provisions the pinned native MSVC Rust
toolchain, its matching rust-src component and offline dependency cache. Build
the Cargo bootstrap binary with all output locations private to the checkout.
These are temporary process environment settings managed by agents, not files
or configuration the CEO must create:

```powershell
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'target/bootstrap'
$env:CARGO_BUILD_TARGET_DIR = $env:CARGO_TARGET_DIR
$env:CARGO_BUILD_BUILD_DIR = $env:CARGO_TARGET_DIR
cargo build --offline --locked --target x86_64-pc-windows-msvc -p xtask --bin susi-dev
```

Before doing this, the agent verifies that the checkout and target paths are
real directories, not redirected shared outputs. The temporary bootstrap is
compiler-side development infrastructure, not a qualified distributable. Its
Cargo wrapper supplies the Rust Windows entry point and the authenticated
source verifier's hashing/JSON dependencies. Compiling susi-dev.rs directly with
rustc is no longer a supported bootstrap path.

```text
target\bootstrap\x86_64-pc-windows-msvc\debug\susi-dev.exe build debug
target\bootstrap\x86_64-pc-windows-msvc\debug\susi-dev.exe workflow start worker-a --scope crates/susi-paths
target\bootstrap\x86_64-pc-windows-msvc\debug\susi-dev.exe workflow integrate worker-a
target\bootstrap\x86_64-pc-windows-msvc\debug\susi-dev.exe release qualify
target\bootstrap\x86_64-pc-windows-msvc\debug\susi-dev.exe release status
```

The qualified bootstrap target is native x64 Windows/MSVC. Other platforms need
their own standard-library and OS-boundary qualification. Arguments are passed
directly to executables, without shell interpretation.

Product build operations execute the reviewed private standard-library builder,
authenticate the complete pinned upstream source and permitted replacements,
and run the consumer test. The resulting archive handles remain locked through
compilation and are rechecked afterward. Compiler and rustdoc use the same
verified selection. A saved receipt or environment variable cannot authorize a
different standard library. Production qualification also checks all packaged
helper executables; the initial bootstrap is never substituted for them.

The launcher rejects a redirected `target/` or launcher artifact directory,
including Windows junctions. It overrides `CARGO_TARGET_DIR`,
`CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, and `CARGO_BUILD_TARGET` in the
child process only. Cargo's intermediate build directory is independent of its
final target directory, so both are set explicitly. User and repository Cargo
configuration files are not rewritten.

The older `cargo local-*` aliases remain convenient when the host's Cargo output
settings are already isolated. They cannot protect the initial Cargo compilation
before their Rust code begins; the standalone launcher closes that bootstrap gap.

Build output isolation does not turn cooperating agents into an OS security
sandbox. Integration, license qualification, artifact verification and rollback
remain the responsibilities of the commands the launcher invokes. Failed gates
are propagated without publishing or installing an unverified production artifact.

Verified local releases use persistent state under target/local-releases/runtime and reserve a private port range in the same native locked global allocator as development instances. Promotion and rollback reuse that repository's range. They never attach to or restart the separately installed production daemon on its default ports; switching the selected package takes effect on the next explicit run. No executable already running is overwritten.
