# Vendored crates

Unmodified crates.io sources plus a minimal, documented patch, wired in via
`[patch.crates-io]` in the root `Cargo.toml`. They used to live here as path
crates; they now live on the `nopr/vendor-patches` branch of this repository
(https://github.com/intellibitz/susi/tree/nopr/vendor-patches), and the root
`Cargo.toml` pins one of its commits with `rev = "..."`.

**Why a git source and not a path:** a path crate's build embeds the worktree's
path, so it and every crate above it (ra2a, rmcp, tokenizers, fastembed, the
candle crates — the heaviest compiles in the tree) recompiled in every new
worktree, and could sit in neither the seeded target snapshot
(`scripts/seed-target.sh`) nor the machine's compiler cache
(`scripts/setup-sccache.sh`). A git revision is the same key in every worktree
and on every machine: the chain is built once and copied or served after.

**Changing a patch:** check out the branch, commit the change there, push it
(`git push origin <sha>:refs/heads/nopr/vendor-patches` — a `nopr/` branch
opens no pull request and never merges), then bump the two `rev`s in the root
`Cargo.toml` on a task branch like any other change. Never delete or rewrite
the branch: every revision `main` ever pinned must stay fetchable. `deny.toml`
allows this repository as a git source for exactly these two crates' sake.

The branch's README.md carries the per-crate notes below as well.

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
