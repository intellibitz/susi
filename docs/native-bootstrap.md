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

From an agent's own checkout on Windows, compile it once with the provisioned
native MSVC Rust compiler:

```text
rustc --edition=2021 xtask/bootstrap/susi-dev.rs -o xtask/bootstrap/susi-dev.exe
```

The output path exists in every checkout, is private to that checkout, and is
ignored by Git. Direct rustc does not use Cargo's target-directory environment
settings. Recompile this small dependency-free executable when its source changes.

```text
xtask\bootstrap\susi-dev.exe build debug
xtask\bootstrap\susi-dev.exe workflow start worker-a --scope crates/susi-paths
xtask\bootstrap\susi-dev.exe workflow integrate worker-a
xtask\bootstrap\susi-dev.exe release qualify
xtask\bootstrap\susi-dev.exe release status
```

On Unix, omit `.exe` in the compilation output and invoke the result with `./`.
The launcher selects the native compiler host automatically; Windows requires
MSVC. Arguments are passed directly to executables, without shell interpretation.

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
