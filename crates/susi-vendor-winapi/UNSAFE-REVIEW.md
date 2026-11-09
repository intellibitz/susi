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
`AddAccessAllowedAce`, `InitializeSecurityDescriptor` /
`SetSecurityDescriptorDacl` / `SetSecurityDescriptorControl` with
`CreateFileW` (creation with the list already applied), and
`OpenProcessToken` / `GetTokenInformation` for the process token user.

- Atomic owner-only creation (2026-11-09, security remediation after an
  independent review): `open_owner_only_append` (`OPEN_ALWAYS`, the access
  `OpenOptions::append` requests: `FILE_GENERIC_WRITE` without
  `FILE_WRITE_DATA`) and `create_new_owner_only` (`CREATE_NEW`,
  `FILE_GENERIC_WRITE`) pass an absolute security descriptor holding the
  owner-only list in `SECURITY_ATTRIBUTES` to `CreateFileW`, so a file the
  call creates exists only with that list: no handle (a concurrent appender
  or a reader holding the file open, which a later `SetNamedSecurityInfoW`
  does not revoke) can be opened against the parent's inherited list, and a
  failure (no token user, a list that cannot be built, the create itself)
  creates nothing, so no caller ever deletes a path by name to clean up. The
  share mode is `std::fs`'s (read, write, delete: readers, other appenders
  and a rotating rename keep working); `bInheritHandle` is FALSE; the handle
  is checked against `INVALID_HANDLE_VALUE` and moved once into an owned
  `File`. The descriptor, the `SECURITY_ATTRIBUTES` and the ACL buffer it
  points into are owned locals that outlive the synchronous call (an absolute
  descriptor stores the DACL pointer, it does not copy the list); the
  descriptor is not touched between initialisation and the call. An existing
  file is opened as it is (Windows ignores the descriptor then): only files
  these functions create are guaranteed owner-only. `SE_DACL_PROTECTED` is set
  explicitly; a mutation run showed Windows also stores an explicit creation
  list as protected without it, so the bit is defense in depth and the tests
  check the stored outcome (protected control bit, exactly one non-inherited
  entry, unchanged when the parent later propagates a new inheritable entry),
  not the bit's necessity. The ACL construction is shared with
  `set_protected_dacl` (`build_acl`), unchanged.

- The DACL is protected (`PROTECTED_DACL_SECURITY_INFORMATION`) with one
  explicit `FILE_ALL_ACCESS` entry for the token user, the access list
  `icacls <path> /inheritance:r /grant:r <user>:F` produced. It never widens
  access and never uses the `USERNAME` variable.
- The ACL buffer is a sized `u32`-aligned owned allocation that outlives the
  call; the security descriptor returned by `GetNamedSecurityInfoW` is copied
  into owned `Ace` values and released with `LocalFree`; the token handle is
  closed on every path. SIDs are validated (`IsValidSid`, `GetLengthSid`)
  and bounds-checked against the ACE size before they are read.
- No raw pointer, raw handle or descriptor crosses out of the module (a
  created file leaves only as an owned `std::fs::File`). Errors are
  `io::Error` from the Win32 status; a failure to restrict a secret is
  reported, not ignored, by callers that require it.
- Fail-closed reads: `read_dacl` lists only allow and deny entries, each as
  stored (an inherit-only entry is listed like any other, so the reader can
  only over-report). Every other entry type fails the read with the typed
  `UnsupportedAce` error (index and type byte, an `io::Error` of kind
  `Unsupported`): compound, object, callback, any type newer than this module,
  and also the audit, alarm, mandatory-label, resource-attribute,
  scoped-policy, trust-label and access-filter types, which belong to a SACL,
  not a DACL, so one found in a DACL is not a list this module can vouch for
  (an earlier revision skipped them as harmless; the type table is deleted).
  An ACL header whose revision is not `ACL_REVISION` (2) or `ACL_REVISION_DS`
  (4) fails with `InvalidData` before any entry is walked. A list that omitted
  an entry able to grant access therefore never reads as restrictive, and the
  readiness scan in `susi-daemon` reports such a file as an unverifiable
  credential finding rather than a clean one. The one new production `unsafe`
  expression reads the header revision next to the entry count that was
  already read (same `// SAFETY:` justification); geiger counts below were
  regenerated.
- Tests (real files, real Win32, plus in-memory ACLs): the DACL after
  `restrict_to_current_user` is exactly one non-inherited `FILE_ALL_ACCESS`
  entry for the token user (the owner can still rewrite and delete the file);
  `set_protected_dacl` holds the requested grants in order; SID strings
  round-trip through the binary form and malformed strings are refused; a
  missing path and an embedded NUL are errors, not panics; real files whose
  DACL holds an object allow, a callback allow or a callback deny entry
  (planted with `AddAce` at `ACL_REVISION_DS`) fail `read_dacl` with
  `UnsupportedAce { index, ace_type }`; an in-memory ACL with each other
  entry type byte that `AddAce` accepts (at least the 20 documented types)
  behind an allow entry fails the walk with that index and type; ACL revisions 0, 1, 3, 5 and 0xFF fail with
  `InvalidData` while 2 and 4 walk; an inherit-only entry is listed; a raw
  allow/deny pair still lists exactly.
- Callers (`susi-core` bus secret, `susi-error` metrics sink) create their
  files through the atomic functions above. The previous revision created the
  file with `create_new`, then set the list by name: a concurrent writer that
  found the file existing could append (and a reader could open a handle that
  keeps reading the secret written later) before the list was set, and on a
  failed restriction the sink removed the path by name, which could delete a
  replacement another writer had made there after a rotation. Now the sink
  creates-or-opens in one call and never removes anything; the bus secret
  uses `create_new_owner_only`, adopts the winner's key when it loses the
  race (`AlreadyExists`) and withholds the key (empty, matching no presented
  key) on any other failure, with nothing created. `restrict_to_current_user`
  keeps no production caller; it stays as the reference list the daemon's
  readiness-scan tests compare against and as their fixture.
- Tests added: a created file is owner-only while still open and before any
  write (both functions); a second appender opening while the creator holds
  the file sees the owner-only list and both lines land; a rename while both
  handles are open succeeds and the next open makes an owner-only
  replacement; the stored descriptor is protected; a later inheritable entry
  propagated from the parent reaches a plain file but not a created one; an
  existing file keeps its list and `CREATE_NEW` refuses it; injected failures
  (an invalid SID in the list, an embedded NUL, a directory or a missing
  parent at the path) create nothing and leave an existing file's bytes and
  list unchanged. In `susi-error` the opener is injectable: an open that fails
  after another writer rotated the file and made a replacement leaves both
  files untouched (a mutation that restores the old remove-on-failure makes
  this test fail), and a failed open of a new sink leaves no file.

## Executed evidence

`cargo test -p susi-vendor-winapi --offline --lib` passed all three tests, including
actual detached child argument round-trips, caller EOF while the child remains
alive, invalid input and missing executable errors. The test releases its bounded
owned child before checking the EOF timing assertion.

`cargo clippy -p susi-vendor-winapi -p susi-gawd --offline --lib --tests -- -D warnings`
passed. The integrated seed fixture regression remains a separate required gate.

`unsafe-review.json` is actual cargo-geiger (0.14.0) output, regenerated
2026-11-09 after the atomic owner-only creation (174 unsafe expressions
before it, 189 after it, all 15 in production code: the descriptor
initialisation, DACL and control calls, `CreateFileW` and the handle
adoption; 31 safe functions before, 35 after). Geiger builds without
`cfg(test)`, so test modules are not counted: a scan of the same copy with
every test module stripped gives the identical 189 (and 174 for the previous
`acl.rs`). An earlier note here said the scan includes test helpers; it does
not. The scan is of a byte-identical standalone copy of this
facade (all five modules: acl, detached, environment, host, lib), with the
same winapi dependency and features. Only workspace
lint inheritance was replaced by an empty `[workspace]` to isolate the scan from
unrelated prototype trees. Invocation:

```
cargo geiger --manifest-path <isolated-copy>/Cargo.toml --target x86_64-pc-windows-msvc --all-dependencies --offline --output-format Json
```

First-party counts (whole facade): **0 unsafe functions, 189 unsafe expressions,
0 unsafe impls, 0 unsafe traits, 0 unsafe methods** (35 safe functions, 4 safe
impls, 4 safe methods). The previous file (38 unsafe expressions) predated the
`acl`, `host` and `environment` modules and was stale; the count includes
expressions inside justified blocks. The `winapi` dependency itself reports 87
used unsafe expressions and 9 used unsafe functions. No ratchet baseline
changed.

Reviewed SHA-256 (UTF-8, LF):

- `src/detached.rs`: `08ad0c1994c531ad00a111abb48a0c3e257dfcfa892614eedcd344467aa37140`
  (the previously recorded `3aa44740...` was stale: `detached.rs` changed in
  582d887c9 after that review)
- `src/acl.rs`: `0bd041cf942d9392d748cdbb09e7be1d75551b0fdb94b8dcd8b3e52fae4d30e7`
  (atomic owner-only creation; previously `ed69183a...`)
