# Windows product standard-library prototype

This is an original Rust prototype, not a release-qualified toolchain. It replaces
the pinned Rust standard library's Unicode data implementation with calls into
Windows ICU. No Unicode character database is copied into the replacement.
Upstream licenses and source-file hashes remain recorded for unchanged files.

The only supported compiler is native x64 MSVC Rust 1.99.0, commit
`b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`. The only qualified repertoire candidate
is Windows ICU Unicode 15.1.0. The backend checks the OS version before its first
Unicode operation and refuses version drift. `char::UNICODE_VERSION` reports
15.1.0. This product variant intentionally removes const evaluation from
`char::is_lowercase`, `char::is_uppercase`, and `char::is_whitespace`; their runtime
counterparts remain. This is not a general substitute for the compiler's normal
Unicode17 standard library. Source using those three APIs in const contexts must
fail compilation, rather than receive different compile-time and runtime results.

`cargo run --manifest-path toolchain/windows-stdlib/Cargo.toml --offline -- <fresh-directory>`
copies the installed pinned rust-src into a private directory, validates the
Unicode source directory, replaces its only implementation file, and builds the
sysroot offline. Bootstrap is limited to the child standard-library build; normal
consumers use stable rustc with `--sysroot <fresh-directory>/sysroot`. Only selected
rlibs are installed, never the separately built std DLL or its CRT startup.
The receipt explicitly says `requires-qualification` and cannot authorize a
production release. Existing destinations are preserved and refused.

The source tree contains only `core/src/unicode/mod.rs` and `unicode_data.rs`.
The former retains its original Apache/MIT license. The latter is replaced
wholesale. The actual Windows sysroot dependency graph excludes foldhash,
unicode-ident and fortanix-sgx-abi; all-target upstream notices still mention
them and must not be edited to pretend otherwise.

`qualify.rs` independently compares all eleven replacement properties and all
four case conversions against the installed, unmodified Rust17 implementation
for every scalar assigned by Windows. Compile this audit-only fixture with the
pinned compiler's `unicode_internals` feature enabled in a scoped subprocess;
it never enters product builds. The initial comparison examined 287,346 scalars
and found 71 Unicode-version differences. This records a platform contract,
not Unicode17 parity. Numeric category tests distinguish numeric characters
from Han characters that merely have numeric values. `consumer.rs` runs ordinary
stable-consumer tests for casing expansion, locale independence, supplementary
characters, UTF-16 errors, formatting, threading, hashing, and unwinding.

Pending gates: complete product and helper rebuilds against the exact final
sysroot; production link-map attribution; automatic admission tied to reviewed
builder/source/notice hashes; source snapshot provenance validation; native
startup lifecycle and installed first-goal testing; clean supported-Windows VM
qualification. Until those pass, do not promote or claim license completion.
