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
| Foreign declaration blocks | 3 | Two Windows system imports, two OS UCRT imports, and the pinned compiler-generated C main match the qualified x64 calling convention. |
| Function bodies | 5 | `atexit` forwards a lifetime-valid callback; `boundary_failure` terminates through the OS; `image` reads loader-validated PE headers; `visit` bounds and aligns every callback slot; `susi_pe_entry` runs exactly once and retains Rust main/runtime initialization. |
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
