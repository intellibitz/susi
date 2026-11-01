# Unicode platform boundary

Susi's IDNA adapter contains Rust implementation code under Apache-2.0 OR MIT.
It distributes no Unicode property or mapping tables and no ICU implementation.
Its only dependency is MIT-licensed `libloading`, which invokes the installed
operating system's library loader. This is the OS ABI boundary allowed by the
CEO mandates, not a relicensing of Unicode or ICU.

The integration patch is `idna_adapter = { path = "vendor/idna-adapter" }` in
`[patch.crates-io]`, keeping version 1.1.0. It removes the adapter's edges to
`idna_mapping`, `unicode-normalization`, `unicode-bidi`, and
`unicode-joining-type`. Other dependency paths must still be audited separately.

## Implementation and platform contract

Rust implements the current nontransitional UTS46 mapping derivation using
OS normalization and character-property queries, followed by OS NFC. It does
not call the OS's older whole-domain IDNA implementation: that would retain
historical exclusions for Georgian capitals and Cyrillic palochka. The existing
Rust `idna` crate still handles Punycode, contextual checks, bidi validation,
DNS lengths and the caller's ASCII deny policy. Validation rejects ignored
characters where the upstream adapter requires rejection.

Windows loads `icu.dll` with `LOAD_LIBRARY_SEARCH_SYSTEM32`, never the current
directory or PATH. macOS uses `/usr/lib/libicucore.A.dylib`. Linux discovers
versioned `libicuuc.so` libraries only in standard absolute system library
directories and resolves their matching ICU C symbol suffix. Neither a custom
configuration nor a library download is attempted. Unavailable libraries,
missing symbols and missing normalization data yield a typed `PlatformError`.
The upstream infallible IDNA adapter methods reject Unicode input on failure;
they do not silently accept unvalidated domains.

`platform_status()` reports the loaded library and actual Unicode version.
The supported character repertoire and character properties follow that OS.
Unassigned characters are rejected, even if a newer Unicode release assigns
them. This is deliberately not a promise of Unicode 18 equivalence. OS upgrades
can extend the repertoire or change properties and require release qualification.
The public XID helper functions expose OS properties separately for source-code
tooling; they do not assert equivalence to a particular Rust compiler version.

## Qualification on 2026-11-01

Windows supplied Unicode **15.1.0.0**. Eight focused tests pass, including modern
mapping regressions, disallowed input, joiner/bidi property masks, unsupported
repertoire, missing-library and wrong-library errors. Clippy passes with warnings
denied. Linux x86_64 and macOS x86_64 `cargo check` pass. Their dynamic loading and
runtime behavior have **not** been tested on those operating systems.

An isolated, non-distributed comparison harness checked all **1,112,064** Unicode
scalars against the previous registry adapter 1.1.0. Mapping and validation had
**zero differences for OS-assigned scalars**. There were 5,185 differences solely
for scalars unassigned by this OS; these are rejected rather than guessed.
The selected property comparison (mark, virama and LTR) found six differences on
assigned scalars: U+1171E and U+1D6C1/U+1D6FB/U+1D735/U+1D76F/U+1D7A9. These remain
OS property values; no copied-data overrides were introduced. The mathematical
symbols map before bidi validation. These measurements are not a full claim of
property equivalence across Unicode versions.

The same harness compared `domain_to_ascii` success/error/output for all **6,387**
scalar-valid source strings in the cached Unicode 16 IdnaTestV2 corpus:
**zero differences**. This is differential testing, not a claim that every
possible flag combination or malformed UTF-16 corpus case was exercised. Corpus
SHA-256: `ffcf59f6e97d765caa5ac8315e9511ba3b42955a554cbc431e4c016f20d25ca9`.
The reference data was read in the qualification environment, not copied into
Susi's repository or distribution. End-to-end combined-tree verification remains
the integrator's responsibility.

## Primary references

- [Microsoft system ICU APIs and Windows availability](https://learn.microsoft.com/en-us/windows/win32/intl/international-components-for-unicode--icu-)
- [ICU C normalization API](https://unicode-org.github.io/icu-docs/apidoc/released/icu4c/unorm2_8h.html)
- [ICU C property API](https://unicode-org.github.io/icu-docs/apidoc/released/icu4c/uchar_8h.html)
- [ICU library naming](https://unicode-org.github.io/icu/userguide/icu4c/packaging.html)
- [ICU versioned ABI design](https://unicode-org.github.io/icu/userguide/icu/design)
- [UTS46 mapping derivation and version changes](https://www.unicode.org/reports/tr46/)
- [Unicode licensing terms](https://www.unicode.org/copyright.html)

These sources explain API contracts and protocol behavior. They do not grant an
Apache/MIT/BSD license to redistribute Unicode data. No such license is claimed.
