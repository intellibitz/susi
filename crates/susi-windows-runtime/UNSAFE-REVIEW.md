# First-party Windows ABI exposure review

The crate denies unsafe code by default and denies unsafe operations inside
unsafe functions unless each operation has an explicit unsafe block. It has no
module-wide unsafe allowance. Exceptions are attached only to reviewed function
bodies, foreign declaration blocks, one `Sync` implementation, and individual
PE/ABI metadata statics. Macro-generated exceptions apply to each null sentinel
static, not the macro's surrounding module or its callers.

The reviewed surface after expanding the eight sentinel declarations is:

| Boundary | Items | Safety obligation |
| --- | ---: | --- |
| Foreign declaration blocks | 3 | Two Windows system imports, two OS UCRT imports, and the pinned compiler-generated C main match the target's system calling convention (qualified on x64; on ARM64 `extern "system"` is the C convention). |
| Function bodies | 5 | `atexit` forwards a lifetime-valid callback; `boundary_failure` terminates through the OS; `image` reads loader-validated PE headers and refuses a COFF Machine other than the build architecture; `visit` bounds and aligns every callback slot; `susi_pe_entry` runs exactly once and retains Rust main/runtime initialization. |
| `Sync` implementation | 1 | Immutable loader metadata contains pointers; only the loader writes the separately declared TLS index before application entry. |
| Exported-symbol and section statics | 16 | Unique ABI names, writable TLS markers/index, read-only callback sentinels/directory, and the fail-closed Rust exception descriptor identity have the qualified PE layout. |

Only `atexit` and `susi_pe_entry` require unsafe Rust function signatures. The
termination-table and refused-descriptor callbacks have safe Rust bodies and do
not acquire an unnecessary unsafe exception. Every virtual RTTI destructor call
is refused with process status 255; no general C++ RTTI implementation is claimed.

A native x64 Windows `cargo geiger --all-dependencies --offline --output-format
Json` scan measured **2 unsafe functions, 45 unsafe expressions, and 1 unsafe
implementation** in used code, with zero unused unsafe items. There are no
external crate dependencies. This measurement used an isolated package with
byte-identical `src/lib.rs`, `src/platform.rs`, and `build.rs`; only the scratch
manifest replaced workspace-lint inheritance with a local workspace declaration.
It is a crate-specific measurement, not a substitute for the final selected
production dependency graph scan.

The existing unsafe ratchet classifies path packages as first-party and records
their exposure separately from third-party exposure growth. That existing rule
is unchanged. `.agents/baseline/unsafe-default.json` was not regenerated or edited.
The new exposure must remain visible in current reports and this source review;
it is not zero and must never be hidden behind a fabricated baseline. Installed
Geiger reports use the `unsafe_` JSON key, so readers must preserve that schema
as well as the historical `unsafe` spelling.

Verification includes deny-by-default compilation, Clippy with warnings denied,
bounded-range tests, negative ABI/RTTI cases, and twelve real lifecycle cases
against both unwind and fat-LTO panic-abort builds. Rust `.rlib` linkage with
`use susi_windows_runtime as _;` passes the same lifecycle qualification. Linux
metadata compilation verifies the empty non-Windows crate remains buildable.

## Re-review 2026-11-03: Windows ARM64 source support

CEO decision 2026-11-03 keeps ARM64 Windows support in core source and keeps
it out of core CI and out of public releases. A native ARM64 host, qualified
in the susi-commercial-apps certified-core pipeline, produces the evidence.
The reviewed
files changed as follows; the guard digests in `unsafe-review.json` are re-bound
to the new sources.

- `src/lib.rs`: the compile-time gate is an explicit allow-list of `x86_64` and
  `aarch64` with MSVC, and `platform.rs` is compiled for both. No unsafe change.
- `build.rs`: host must still equal target, now from the same allow-list, and the
  rustc `host:` line must equal the target as well (OSN-22). No unsafe code.
- `src/platform.rs`: `image()` also reads the COFF Machine field (NT headers + 4)
  inside its existing unsafe block, next to the signature, magic and image size
  it already read from the same loader-validated headers, and exits through the
  existing `boundary_failure()` unless it is `0x8664` (x86_64) or `0xAA64`
  (aarch64). SAFETY comments now name the target's system ABI and the PE32+ TLS
  directory shared by x64 and ARM64 instead of x64 only.

No unsafe block, unsafe function, unsafe implementation, foreign declaration,
exported symbol, link section or ABI layout was added or changed. For x86_64 the
unsafe semantics are unchanged: a loader-mapped x64 image always carries
Machine `0x8664`, so the added in-bounds read cannot change an accepted image's
path, and every refusal is the pre-existing fail-closed exit.

Re-measured on native x64 Windows with the same isolated-package Geiger method
(which reproduced the previous 2 / 45 / 1 counts for the previous sources): **2
unsafe functions, 48 unsafe expressions and 1 unsafe implementation** in used
code, zero unused. The three added expressions are the one Machine read. The
aarch64 build is reviewed, not executed: the review host is x64 and cannot run
ARM64 code. ARM64 is not qualified until a native ARM64 host passes the same
lifecycle and OS-UCRT fixtures.

## Re-review 2026-11-04: `spawn_without_inheriting_stdio` (D-SP-4 Stage 3)

`spawn_without_inheriting_stdio` is added to `src/lib.rs` (matching
`crates/susi-daemon/src/server.rs` in semantics) so OS-UCRT binary targets
(`installed_channel` and the thin client) share the no-inherit Windows process
spawn without pulling in `susi-daemon` or `winapi`. (`susi-daemon` keeps its
library-local copy so `msvcrt`-linked library and integration test harnesses do
not link `libsusi_windows_runtime.rlib` and collide on `atexit`.)

- `src/lib.rs`: declares `GetHandleInformation` and `SetHandleInformation` in a
  `#[link(name = "kernel32")] extern "system"` block on allow-listed Windows
  MSVC targets and wraps the two handle-flag calls in narrow `unsafe` blocks
  inside safe `spawn_without_inheriting_stdio`. Null standard handles are
  skipped, invalid or pseudo-handles return `0` (`FALSE`) without mutation, and
  any cleared `HANDLE_FLAG_INHERIT` bit is restored immediately after
  `cmd.spawn()`. Non-Windows targets execute `cmd.spawn()` with no unsafe code.
- `build.rs` and `src/platform.rs`: byte-identical (`802DF3AE...` and
  `F1B6F0B5...`).

Re-measured on native x64 Windows with `cargo-geiger 0.14.0` using the same
isolated-package method: **2 unsafe functions, 61 unsafe expressions, and 1
unsafe implementation** in used code (7 safe functions, 117 safe expressions),
with zero unused unsafe items and zero external crate dependencies.
