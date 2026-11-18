# Product standard-library builder

An original Rust tool, not a release-qualified toolchain. It replaces the pinned
Rust standard library's Unicode data implementation with calls into the host's
OS ICU, so no Unicode character database is copied into Susi core.
Upstream licenses and source-file hashes remain recorded for unchanged files.
Core qualifies Linux only (Mandate 84); the contracts, prerequisites and checks
are in [NATIVE-BUILDS.md](NATIVE-BUILDS.md). `platform_unicode.rs` keeps its
portable source for other targets, which core does not build or qualify.

The only supported compiler is Rust 1.99.0, commit
`b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`. This product variant intentionally
removes const evaluation from `char::is_lowercase`, `char::is_uppercase`, and
`char::is_whitespace`; their runtime counterparts remain. Source using those
three APIs in const contexts must fail compilation, rather than receive
different compile-time and runtime results.

`cargo run --manifest-path toolchain/stdlib/Cargo.toml --offline -- <fresh-directory>`
copies the installed pinned rust-src into a private directory, validates the
Unicode source directory, replaces its only implementation file, and builds the
sysroot offline. Bootstrap is limited to the child standard-library build; normal
consumers use stable rustc with `--sysroot <fresh-directory>/sysroot`. The
receipt says `requires-qualification` and cannot authorize a production release.
Existing destinations are preserved and refused. Core's build tooling runs it
through `xtask/src/trusted_sysroot.rs`, which hashes these sources into the
sysroot identity and recompiles `consumer.rs` against every admitted sysroot.

The source tree contains only `core/src/unicode/mod.rs` and `unicode_data.rs`.
The former retains its original Apache/MIT license. The latter is replaced
wholesale. All-target upstream notices still mention crates the sysroot
dependency graph excludes and must not be edited to pretend otherwise.
