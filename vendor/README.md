# Vendored patches

These exact sources were copied from revision `c69a90d60e8e7d96ce62d83c3e3d0d631b970480` of the existing vendor-patches branch. They now ship in the checkout, including upstream license notices, so source resolution requires no GitHub connection.

Root Cargo patches use local paths. This deliberately replaces the previous git-source caching optimization with source availability. Per-worktree build isolation remains mandatory; cross-worktree cache performance must be measured again. Update the sources and their patch notes together, preserving original licenses. Vendoring does not make non-Rust dependencies Rust or change their licenses.

## native-tls 0.2.18

`native-tls` 0.2.18 is patched for Windows private-key ownership and
certificate-chain handling (`ureq` / `tokio-native-tls` platform TLS; the
retired `reqwest` and `fluent-uri` vendor trees were removed after
`T-CLAUDENOA2ASDK-1` and the std JSON-schema cut dropped their last consumers).
See `native-tls/SUSI-PATCHES.md` for qualification, limitations and upstream
source provenance. Non-Windows platform TLS boundaries must be evaluated
separately; Windows evidence does not certify Linux builds.

## esaxx-rs 0.1.10

**Why:** tokenizers enables `esaxx-rs/cpp` via its default `esaxx_fast`
feature, and esaxx-rs 0.1.10 (unmaintained — latest release) hardcodes
`.static_crt(true)` in build.rs. On MSVC that emits `/MT`, while every other
C/C++ object in the tree (onnxruntime, zstd-sys, …) links `/MD`; mixing CRT
models fails at link with `LNK2038: mismatch detected for 'RuntimeLibrary'`.
On non-MSVC toolchains `static_crt` is a no-op, so dropping it is a
Windows-only change.

**Patch** (diff against the crates.io tarball):

- `build.rs`: `.static_crt(true)` removed from the non-macOS `cpp` branch.

**Upgrading:** copy the new release from `~/.cargo/registry/src/*/esaxx-rs-X.Y.Z`
onto the branch, re-apply the edit above, and update this section.

## Rust dependency replacements (2026-11-01)

The following sources were copied from the crates.io registry cache at their
exact published versions. Original copyright and license files are retained.
These patches apply to the resolved CPU build; optional GPU features are not a
claim of Rust-only qualification.

- **hashbrown 0.16.1 and 0.17.1**: replace the default foldhash hasher with
  ahash 0.8.12 (`runtime-rng`, no defaults). This removes the Zlib-only default
  implementation without changing the map API. Hash values/order are not a
  persistent storage format or a compatibility promise.
- **zip 9.2.0**: default deflate selects flate2's Rust backend instead of
  zlib-rs. Other explicitly selected backends still require their own audit.
- **candle-core 0.11.1**: replace yoke ownership of safetensors buffers with
  self_cell 1.3.0. Memory maps and owned byte buffers remain self-referential,
  zero-copy owners; parsing and tensor access use the original SafeTensors API.
- **tantivy-sstable 0.7.0**: replace zstd's C implementation with ruzstd 0.9.1.
  Writes remain standard Zstandard frames, using Fastest compression; decoding
  is bounded to 64 MiB per dictionary block. Compression ratio and throughput
  are not assumed identical. The facade test writes and reopens 2,500 terms.
  The published crate omitted the repository-level attribution files. `LICENSE`
  and `AUTHORS` are restored from its recorded upstream commit
  `9e63fc508153ef770f9ff980c8fa2f11e8e2e6db` in `quickwit-oss/tantivy`
  (upstream Git blobs `7b1fc849b042df2e7607138009e81bd078d9db4a` and
  `59562653b4c2e88837c3293e1dd0e16e840d1ff8`).

When upgrading any patch, compare the complete upstream source, retain notices,
reapply only the documented changes, and rerun the strict graph audit and real
format/inference round trips. A path patch never changes upstream licensing.
