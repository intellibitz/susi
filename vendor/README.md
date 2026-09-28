# Vendored crates

Unmodified crates.io sources plus a minimal, documented patch, wired in via
`[patch.crates-io]` in the root `Cargo.toml`. Not workspace members
(`exclude = ["vendor"]`), so workspace lints do not apply to upstream code.

## reqwest 0.13.5

**Why:** the whole tree uses ring as its rustls crypto provider. Upstream
reqwest's `rustls` feature (and therefore `default-tls`/`default`) hard-wires
aws-lc-rs, whose `aws-lc-sys` C build was the cold-build critical path
(~285s of a ~8.5min `cargo build --timings` on 28 cores). Our own crates could
opt out, but `ra2a` (reqwest default features) cannot. (`wasmer-wasix` also
requested `rustls` until susi-native moved it to `sys-minimal`.) Switching to
`rustls-no-provider` instead would make every `Client::new()` panic unless a
provider was installed first — including clients third-party crates build
internally — so the provider choice is patched here, once.

**Patch** (diff against the crates.io tarball):

- `Cargo.toml`: new feature `__rustls-ring` (`hyper-rustls?/ring`,
  `tokio-rustls?/ring`, `rustls?/ring`, `quinn?/rustls-ring`);
  `rustls` enables `__rustls-ring` instead of `__rustls-aws-lc-rs`.
- `src/async_impl/client.rs`: `default_rustls_crypto_provider()` returns
  `rustls::crypto::ring::default_provider()` under `__rustls-ring`.

`Cargo.lock` from the tarball is dropped (unused for a dependency).

**Invariant:** `cargo tree -i aws-lc-rs` must report no match. Two providers
compiled into rustls make its automatic provider selection ambiguous.

**Upgrading:** copy the new release from `~/.cargo/registry/src/*/reqwest-X.Y.Z`,
re-apply the two edits above, and update this section.
