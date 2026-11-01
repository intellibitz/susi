# Native detached process boundary

The original Apache-2.0 `detached::spawn` safe wrapper is the only first-party
unsafe allowance in this facade. The crate denies unsafe code by default.
The caller in `susi-gawd` retains `forbid(unsafe_code)`.

The wrapper uses the installed Windows operating system. It bundles no process
launcher, shell, executable, or foreign runtime implementation. References:
[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw),
[argument parsing](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments),
[CompareStringOrdinal](https://learn.microsoft.com/en-us/windows/win32/api/stringapiset/nf-stringapiset-comparestringordinal).

## Boundary review

- Absolute executable and working-directory paths; explicit application pointer
  avoids executable-search ambiguity. NUL and invalid executable quotes rejected.
- Owned UTF-16 buffers remain alive throughout the synchronous call. Command-line
  arguments use Windows backslash/quote escaping, including empty and non-ASCII
  arguments. The complete command line has the documented 32,767-unit bound.
- Full explicit environment, with NUL/invalid-name/duplicate-key rejection.
  OS ordinal case-insensitive sorting avoids shipping Unicode tables. Comparison
  errors fail closed. Legal drive-current-directory entries are preserved.
- `bInheritHandles=FALSE` prevents both standard and unrelated inheritable handles
  from crossing the boundary. No process-global handle flags or environment are
  modified. Detached/new-group flags and `SW_HIDE` avoid an interactive window.
- POD startup/output structures are zero initialized; the startup size is exact.
  Successful calls return two owned handles, both closed immediately. The API
  reports creation only; the child opens its own log and completion is observed
  by the existing workflow marker protocol.
- Seed callers remove all three inherited Cargo output-directory variables
  case-insensitively, retaining compiler, rustdoc and verified-sysroot context.

## Executed evidence

`cargo test -p susi-vendor-winapi --offline --lib` passed all three tests, including
actual detached child argument round-trips, caller EOF while the child remains
alive, invalid input and missing executable errors. The test releases its bounded
owned child before checking the EOF timing assertion.

`cargo clippy -p susi-vendor-winapi -p susi-gawd --offline --lib --tests -- -D warnings`
passed. The integrated seed fixture regression remains a separate required gate.

`unsafe-review.json` is actual cargo-geiger output for a byte-identical standalone
copy of this facade, with the same winapi dependency and features. Only workspace
lint inheritance was replaced by an empty `[workspace]` to isolate the scan from
unrelated prototype trees. Invocation:

```
cargo geiger --manifest-path <isolated-copy>/Cargo.toml --target x86_64-pc-windows-msvc --all-dependencies --offline --output-format Json
```

First-party counts: **0 unsafe functions, 38 unsafe expressions, 0 unsafe impls,
0 unsafe traits, 0 unsafe methods**. There are two explicitly justified unsafe
blocks within the one safe wrapper. The count includes expressions inside those
blocks, not just the seven Windows/POD operations. No ratchet baseline changed.

Reviewed `src/detached.rs` SHA-256 (UTF-8, LF):
`3aa44740bf998a5b7191a3b4169881407e38df6e0c4b7a6987705a7f0c7a4f6e`.
