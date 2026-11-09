# Native detached process boundary

The original Apache-2.0 `detached::spawn` safe wrapper and the `acl` module
(native owner-only file ACLs, added 2026-11-09) are the first-party unsafe
allowances reviewed here; `host` and `environment` are also in the scan below.
Every allowance is per-function with a `// SAFETY:` justification. The crate denies unsafe code by default.
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

## Native owner-only ACLs (`acl`)

`acl` replaces launching `icacls.exe`: from the windowless detached daemon each
launch opened a visible console window (CEO report, 2026-11-09, `node.key`).
It calls only the installed OS: `SetNamedSecurityInfoW` /
`GetNamedSecurityInfoW` (file object DACL), `InitializeAcl` /
`AddAccessAllowedAce`, and `OpenProcessToken` / `GetTokenInformation` for the
process token user.

- The DACL is protected (`PROTECTED_DACL_SECURITY_INFORMATION`) with one
  explicit `FILE_ALL_ACCESS` entry for the token user, the access list
  `icacls <path> /inheritance:r /grant:r <user>:F` produced. It never widens
  access and never uses the `USERNAME` variable.
- The ACL buffer is a sized `u32`-aligned owned allocation that outlives the
  call; the security descriptor returned by `GetNamedSecurityInfoW` is copied
  into owned `Ace` values and released with `LocalFree`; the token handle is
  closed on every path. SIDs are validated (`IsValidSid`, `GetLengthSid`)
  and bounds-checked against the ACE size before they are read.
- No raw pointer, handle or descriptor crosses out of the module. Errors are
  `io::Error` from the Win32 status; a failure to restrict a secret is
  reported, not ignored, by callers that require it.
- Fail-closed reads: `read_dacl` lists only allow and deny entries. Entry
  types that never carry access (audit, alarm, mandatory label, resource
  attribute, scoped policy, trust label, access filter) are left out; every
  other type (compound, object, callback, and any type newer than this
  module) fails the read with the typed `UnsupportedAce` error (index and
  type byte, an `io::Error` of kind `Unsupported`), so a list that omitted an
  entry able to grant access never reads as restrictive. The readiness scan in
  `susi-daemon` reports such a file as an unverifiable credential finding
  rather than a clean one. This change adds no `unsafe` expression to
  production code (the classification is safe code), so the geiger counts
  below are unchanged; the new tests use per-function test-only allowances
  that cargo-geiger does not count.
- Tests (real files, real Win32): the DACL after `restrict_to_current_user` is
  exactly one non-inherited `FILE_ALL_ACCESS` entry for the token user (the
  owner can still rewrite and delete the file); `set_protected_dacl` holds the
  requested grants in order; SID strings round-trip through the binary form
  and malformed strings are refused; a missing path and an embedded NUL are
  errors, not panics; real files whose DACL holds an object allow, a callback
  allow or a callback deny entry (planted with `AddAce` at `ACL_REVISION_DS`)
  fail `read_dacl` with `UnsupportedAce { index, ace_type }`; every one of the
  256 entry types is classified and none that can carry access is skipped; a
  raw allow/deny pair still lists exactly.

## Executed evidence

`cargo test -p susi-vendor-winapi --offline --lib` passed all three tests, including
actual detached child argument round-trips, caller EOF while the child remains
alive, invalid input and missing executable errors. The test releases its bounded
owned child before checking the EOF timing assertion.

`cargo clippy -p susi-vendor-winapi -p susi-gawd --offline --lib --tests -- -D warnings`
passed. The integrated seed fixture regression remains a separate required gate.

`unsafe-review.json` is actual cargo-geiger (0.14.0) output, regenerated
2026-11-09 after the `acl` module, for a byte-identical standalone copy of this
facade (all five modules: acl, detached, environment, host, lib), with the
same winapi dependency and features. Only workspace
lint inheritance was replaced by an empty `[workspace]` to isolate the scan from
unrelated prototype trees. Invocation:

```
cargo geiger --manifest-path <isolated-copy>/Cargo.toml --target x86_64-pc-windows-msvc --all-dependencies --offline --output-format Json
```

First-party counts (whole facade): **0 unsafe functions, 170 unsafe expressions,
0 unsafe impls, 0 unsafe traits, 0 unsafe methods** (30 safe functions, 2 safe
impls, 3 safe methods). The previous file (38 unsafe expressions) predated the
`acl`, `host` and `environment` modules and was stale; the count includes
expressions inside justified blocks. The `winapi` dependency itself reports 87
used unsafe expressions and 9 used unsafe functions. No ratchet baseline
changed.

Reviewed SHA-256 (UTF-8, LF):

- `src/detached.rs`: `08ad0c1994c531ad00a111abb48a0c3e257dfcfa892614eedcd344467aa37140`
  (the previously recorded `3aa44740...` was stale: `detached.rs` changed in
  582d887c9 after that review)
- `src/acl.rs`: `28e938d7b12c0e623af4a98ea5419f5288586cfbf51cd3014c80b84b0e00a67c`
