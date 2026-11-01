# Susi OS Unicode backend for regex-syntax 0.8.11

The parser and HIR implementation retain upstream MIT/Apache licensing. This
private product patch replaces the entire generated Unicode tables directory
and table lookup implementation with original Rust queries to installed OS ICU.
No Unicode data files, ICU implementation, or ICU DLL is distributed here.
The independent `susi-os-unicode` crate contains the audited OS loader boundary;
regex-syntax retains `forbid(unsafe_code)`.

Unicode properties, script extensions, general categories, age, segmentation
properties and simple case folding use the installed OS Unicode repertoire.
This is deliberately not a promise of Unicode 16 or 17 on older operating
systems. Unknown properties and unsupported versions return errors; no ASCII
substitution is performed. The OS loader restricts Windows to System32 and Unix
to system directories. Unsupported/missing OS capabilities fail explicitly.
Simple fold equivalence data is queried in memory once per process, never
serialized or shipped. Unicode word-boundary ranges are cached once as well.
The backend exposes its four-component OS Unicode version for qualification.

This is an internal product patch, not a general upstream replacement: the
optional upstream arbitrary/fuzz integration is not part of the shipped API.
The selected product uses std and all Unicode features. The ASCII/no-default
parser still compiles; full OS-backed word boundary caching requires std.

Upstream parser/HIR tests remain, except the fixed Unicode16 Greek script
extension members are replaced with an equivalent set-operation assertion,
because those members changed between OS Unicode versions. The replacement
backend has independent alias, error, surrogate-boundary and simple-fold tests.
The original table-module tests are not distributed with deleted table code.

## Verification and provenance

Upstream archive `regex-syntax-0.8.11.crate` SHA256:
`d6f6ff9a378485b298a5286656da665ba74413d36db0979633275d2e708145d4`.
Original repository revision/path is preserved in UPSTREAM-VCS.json.
The upstream `src/unicode_tables/LICENSE-UNICODE` applies to the omitted
Unicode data directory; neither that directory nor its generated Rust tables
is included in this patched source package.

On Windows with OS Unicode15.1: 143 parser/HIR tests and four backend tests
passed; the std-only ASCII and std+general-category configurations each passed
141 tests. Backend Clippy with warnings denied passed. Upstream parser Clippy
has pre-existing warnings; it is not claimed clean by this patch.
An ignored original-source reference runner compared all1,112,064 Unicode
scalars for each of eight classes. Whitespace, Greek script, Age3.0 and ASCII
simple-fold class were identical. Unicode16 versus OS15.1 differences: word
4433, digit80, letters4302, Greek script extensions9. These observed differences
are repertoire limits, not a claim of equivalence to a newer Unicode release.
