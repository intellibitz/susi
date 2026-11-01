# Original Rust Windows process boundary

This crate supplies original Apache-2.0 Rust implementation and PE metadata.
It is not a wrapper that relicenses or embeds Microsoft CRT startup objects.
The qualified platform is native `x86_64-pc-windows-msvc` with Rust 1.99.0,
commit `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`. `build.rs` rejects another
Windows compiler identity or cross-compilation until it is requalified.

The entry point calls the **compiler-generated C `main(int, char**)`** exactly
once. It does not call a private Rust runtime API or replace Rust `main` with
`no_main`. The compiler's wrapper retains `lang_start`, standard-library setup,
panic handling, main-result conversion, and standard-library cleanup. The
pinned Windows PAL ignores `argc`/`argv`; argument discovery uses
`GetCommandLineW`, so zero/null preserves Unicode, spaces and empty arguments.

The loader receives writable TLS index/storage metadata and the sorted TLS
callback table. MSVC LINK derives the TLS alignment from the merged `.tls`
section; qualification exercises both 64-byte and 4096-byte aligned locals on
the main and worker threads. Incorrect read-only TLS markers are not equivalent:
they split compiler-emitted storage away from the directory's range.

Executable-local `.CRT$XI` callbacks run before `.CRT$XC`; a nonzero XI result
terminates with status 255 before Rust main or later constructors execute.
`.CRT$XP` and `.CRT$XT` callbacks run after normal-return exit callbacks through
an early OS-UCRT registration. All table traversal uses checked integer
arithmetic, image bounds, alignment and a 16,384-slot limit; each pointer is
recovered from the loader-owned mapped address. No pointer arithmetic pretends
that separate Rust marker statics form one Rust allocation.

Normal return runs OS `_cexit` before `ExitProcess`. Pinned Rust's explicit
`std::process::exit` directly calls `ExitProcess`; its existing behavior is
preserved: stack drops and registered C exit/termination callbacks are bypassed,
while Windows FLS thread cleanup still occurs. Abort likewise preserves abort
semantics rather than pretending that destructors ran.

Rust's pinned MSVC unwinder uses static type descriptors and matches exception
names. The required descriptor vtable provides an explicit failing boundary:
**every virtual-destructor invocation exits with 255**. It is not a general
C++ `type_info`, does not advertise foreign RTTI/delete support, and must not be
used to admit C++ implementation into the Rust-only process. Negative fixtures
invoke flags 0 and 1 and verify the refusal. Rust panic/unwind and abort paths
are separately qualified.

## Link contract

The final executable must link this crate (or its separately compiled Rust
object for diagnostics) and use:

- `/ENTRY:susi_pe_entry`
- `/INCLUDE:_tls_used`
- `/NODEFAULTLIB:msvcrt.lib` and `/NODEFAULTLIB:vcruntime.lib`
- `/MERGE:.CRT=.rdata`, preserving sorted read-only callback metadata without
  retaining a separate CRT section or suppressing a warning
- OS-UCRT import metadata plus `ucrt.lib` import metadata
- import metadata for OS `kernelbase.dll`'s qualified `__chkstk` export

An import archive is not evidence that implementation was excluded: inspect the
actual final link map and PE imports. Microsoft `exe_main`, `__scrt_*`, `msvcrt`
and `vcruntime` implementation objects must not reappear. OS capability/version
qualification remains mandatory for exports used by the platform boundary.

## Repeatable native qualification

`src/lib.rs` contains pure range-validation unit tests. `tests/fixtures/driver.rs`
is a standalone Rust executable accepting, in order: the runtime object or Rust `.rlib`,
OS-UCRT import library, OS-stack import library, `lifecycle.rs`, and a fresh
private output directory. The `.rlib` path uses `use susi_windows_runtime as _;` and preserves Cargo-style bitcode linkage. It compiles and runs both unwind and optimized
fat-LTO abort fixtures, with process deadlines, retained logs and link maps.
An opaque diagnostic runtime object must be compiled with `-C opt-level=3
-C panic=abort`; otherwise final fat LTO can remove implicit core panic symbols
still referenced by an unoptimized external object. Normal Cargo integration
uses the Rust crate/bitcode, not an opaque unoptimized object.

The 12 subprocess cases verify normal return 37, explicit process exit 29,
foreign RTTI refusal 255 for both flags, initializer failure 255, uncaught panic
101, and abort fast-fail. Fixtures also exercise XI/XC ordering, XP/XT callbacks,
OS atexit registration, TLS loader callbacks, thread-local/stack drops, Unicode
and empty arguments, stdout/stderr, and a 64 KiB stack probe.

Full Susi final-link, inference, installed journey and release qualification
are additional integration gates; passing these fixtures does not attest them.

## Primary ABI evidence

- [Pinned compiler entry generation](https://github.com/rust-lang/rust/blob/b940084d7eb6a299eb4bfeb8e34901bc051e7ac4/compiler/rustc_codegen_ssa/src/base.rs)
- [Pinned Windows argument acquisition](https://github.com/rust-lang/rust/blob/b940084d7eb6a299eb4bfeb8e34901bc051e7ac4/library/std/src/sys/args/windows.rs)
- [Pinned Windows runtime initialization](https://github.com/rust-lang/rust/blob/b940084d7eb6a299eb4bfeb8e34901bc051e7ac4/library/std/src/sys/pal/windows/mod.rs)
- [Pinned Rust MSVC exception descriptors](https://github.com/rust-lang/rust/blob/b940084d7eb6a299eb4bfeb8e34901bc051e7ac4/library/panic_unwind/src/seh.rs)
- [Microsoft PE entry-point contract](https://learn.microsoft.com/cpp/build/reference/entry-entry-point-symbol?view=msvc-170)
