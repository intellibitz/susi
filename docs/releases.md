# Releases

A Susi core release is two things in git and nothing else: an annotated tag
`vX.Y.Z`, and the committed release record `releases/<tag>.json` on the
default branch. No hosting service's release page, asset list or API is part of
a release (CEO mandate 2026-11-09, "GitHub is storage only"), so the
repositories can move to any git host by changing a remote URL.

Binaries are not committed and are not downloaded. Every install and every
upgrade builds the tag from source on the host that will run it (CEO decision
2026-11-09 00:22).

## The record: `releases/<tag>.json`

Schema `susi-release-record/v1`. `tests/release_records.rs` checks every
record on every gate run.

| Field | Meaning |
|---|---|
| `schema` | `susi-release-record/v1` |
| `product`, `version`, `tag` | `susi`, `X.Y.Z`, `vX.Y.Z`; the file is named after the tag |
| `commit` | the full commit id the tag points at; installers refuse a tag that points elsewhere |
| `tagged` | the annotated tag's date |
| `build.toolchain` | the Rust toolchain of the tag's `rust-toolchain.toml` |
| `build.host` | the host triple the reference binaries were built on |
| `build.profile`, `build.features`, `build.env` | the cargo profile, the feature list and the `NAME=value` environment the reference build used; installers build with exactly these |
| `build.command` | the full command, for people |
| `reproducible` | `false` today: no byte-for-byte reproducibility is claimed |
| `assets[]` | one line per reference binary: `file`, `target`, `package`, `bin`, `sha256` |
| `missing_targets[]` | each released target (x86_64 Windows, x86_64 Linux) with no reference build, and why |
| `checks` | the checks run on the reference build, what passed, what did not run and why; `locally_qualified` says whether the release was qualified on its build host |
| `provenance` | where each fact in the record came from |

The scripts read the record with line tools, because a fresh host may have no
JSON parser. The test therefore also pins the layout: the scalar fields sit on
lines of their own, `env` is one line, each asset is one line, and every value
passed to cargo is a plain word.

### What the sha256 means

The `sha256` of an asset is the digest of the **reference build**: the binary
built when the release was cut, with the recorded command, settings and
toolchain, on the recorded host. It identifies that binary. It is not a gate
for anyone else's build: your own build of the same tag may differ in bytes
(paths, linker, toolchain build, host). Installers report whether a build
matches the reference digest, and they never refuse a build for differing.
**The gate is the version check**: the built binary must report the tag's
version, and the tag must point at the record's commit.

## Cutting a release

Main is unqualified development. A release is cut from a snapshot of it and
qualified on the exact tree it ships (CEO mandate 73):

1. Snapshot main and take the release branch locally:
   `cargo run -p xtask --bin local-snapshot -- cut vX-Y-Z-rcN` (publishes the
   immutable `refs/tags/susi-snapshot/vX-Y-Z-rcN` and creates the local branch
   `release/vX-Y-Z-rcN`).
2. On that branch, commit only the version bump (the engine manifests,
   `Cargo.lock`, the README badge and the governance ledgers) with its task
   record. A defect found from here on is fixed on main and the snapshot is cut
   again (`rcN+1`); the release branch takes nothing else.
3. Qualify: `cargo run -p xtask --bin release-qualify -- vX.Y.Z [--jobs N]` from
   the clean release checkout. It runs `format`, `check`, `clippy`, `licenses`,
   `tests` (the whole workspace), `build` (the reference binaries, profile
   `dist` with `CARGO_PROFILE_DIST_LTO=false` and
   `CARGO_PROFILE_DIST_CODEGEN_UNITS=16`) and `smoke` (each binary answers
   `--version` with the release version from a private home). Only when every
   gate passes does it write `releases/qualification/<tag>.json` (schema
   `susi-release-qualification/v1`: the qualified commit and tree, the
   `Cargo.lock` digest, the toolchain, the host, every gate's command, exit
   code, duration and log digest, the test counts, each asset's sha256 and
   `--version` answer, and the platforms still owed), commit that one file,
   verify the commit and create the annotated tag locally. A failing gate
   writes nothing.
4. Publish: `git push origin vX.Y.Z`. The pre-push `push-policy` admits a
   `vX.Y.Z` tag only when the tagged commit adds exactly its record to the
   qualified tree and the record shows every gate at exit 0 with tests run
   and none failed (`xtask/src/release_qualification.rs`). Every other tag
   name, and any tag replacement or deletion, is refused.
5. Write `releases/<tag>.json` (the record below) with the reference digests
   and land it on main. A platform with no qualification yet is listed under
   `missing_targets` and in the qualification record's `owed_platforms`.

## Installing and upgrading

All three paths resolve the newest tag with `git ls-remote --tags` against a
configured remote, check that tag out, read its record from the remote's
default branch, build `susi` natively with the recorded profile, features and
environment, and install the result only when it reports the tag's version.

They need git and a Rust toolchain installed through rustup. The tag's
`rust-toolchain.toml` pins the compiler (1.99.0 for v0.21.52); when that
toolchain is not installed the installers stop and print the
`rustup toolchain install` command. They never install system software.

| Path | Remote |
|---|---|
| `install.sh` (Linux x86_64) | `SUSI_SOURCE_REPO`, else the `origin` of the checkout the script runs from |
| `install.ps1` (Windows x86_64) | `SUSI_SOURCE_REPO`, else the `origin` of the checkout the script runs from |
| `scripts/susi-release-sync.sh` (upgrades, Windows and Linux; it installs its own next copy from the default branch, not from the tag) | `SUSI_RELEASE_REMOTE`, else the `origin` of the private release checkout `~/.susi/release-src` that the installer or an earlier sync made |

`SUSI_RELEASE_TAG` (installers) and `--tag` (sync) pin a specific release.
There is no built-in default remote: a script that finds none says which
variable to set.

## Commercial certification

`susi-commercial-apps/scripts/refresh-certified-core.sh` fetches the core
repository over git (its configured core remote) and takes the pinned
release's commit and reference digests from `releases/<tag>.json`.
