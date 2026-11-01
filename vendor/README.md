# Vendored patches

These exact sources were copied from revision `c69a90d60e8e7d96ce62d83c3e3d0d631b970480` of the existing vendor-patches branch. They now ship in the checkout, including upstream license notices, so source resolution requires no GitHub connection.

Root Cargo patches use local paths. This deliberately replaces the previous git-source caching optimization with source availability. Per-worktree build isolation remains mandatory; cross-worktree cache performance must be measured again. Update the sources and their patch notes together, preserving original licenses. Vendoring does not make non-Rust dependencies Rust or change their licenses.

## reqwest 0.13.5

**Why:** the whole tree uses ring as its rustls crypto provider. Upstream
reqwest's `rustls` feature (and therefore `default-tls`/`default`) hard-wires
aws-lc-rs, whose `aws-lc-sys` C build was the cold-build critical path
(~285s of a ~8.5min `cargo build --timings` on 28 cores). Our own crates could
opt out, but `ra2a` (reqwest default features) cannot. Switching to
`rustls-no-provider` instead would make every `Client::new()` panic unless a
provider was installed first — including clients third-party crates build
internally — so the provider choice is patched here, once.

**Patch** (diff against the crates.io tarball):

- `Cargo.toml`: new feature `__rustls-ring` (`hyper-rustls?/ring`,
  `tokio-rustls?/ring`, `rustls?/ring`, `quinn?/rustls-ring`);
  `rustls` enables `__rustls-ring` instead of `__rustls-aws-lc-rs`.
- `src/async_impl/client.rs`: `default_rustls_crypto_provider()` returns
  `rustls::crypto::ring::default_provider()` under `__rustls-ring`.

**Invariant:** `cargo tree -i aws-lc-rs` must report no match. Two providers
compiled into rustls make its automatic provider selection ambiguous.

**Upgrading:** copy the new release from `~/.cargo/registry/src/*/reqwest-X.Y.Z`
onto the branch, re-apply the two edits above, and update this section.

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

When upgrading any patch, compare the complete upstream source, retain notices,
reapply only the documented changes, and rerun the strict graph audit and real
format/inference round trips. A path patch never changes upstream licensing.
