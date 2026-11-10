# Provider-neutral qualification inputs

Local development, evidence and release qualification must not need a GitHub
account or service. This document describes supported offline source inputs,
not a claim that clean-machine provisioning or all historical tooling is free
of host assumptions. Agents provision and verify these inputs; the CEO is not
required to write configuration or approve routine engineering steps.

## RustSec advisories

Qualification reads an explicitly provisioned local Git checkout. It never
fetches an advisory database. Supply the checkout with
`SUSI_RELEASE_ADVISORY_DB`, the independently approved complete commit ID with
`SUSI_RELEASE_ADVISORY_REVISION`, and its approved origin identity with
`SUSI_RELEASE_ADVISORY_ORIGIN`. A local mirror or another Git service is valid.
When the origin variable is absent, the existing canonical RustSec origin is
used for compatibility; an empty or malformed value is an error. Merely copying
the checkout's origin into this setting is not independent source approval.
Origin identities must not embed passwords or access tokens; URL credentials,
queries and fragments are rejected before evidence is recorded.

The local origin must match the approved identity, with only a trailing `.git`
normalization. Qualification still requires the approved commit, a clean tree
including ignored files, regular tracked files, a nonempty advisory collection,
and a commit no more than seven days old (with bounded clock skew). An old
offline database does not become current because its mirror is available.
Agents must establish that the approved commit actually belongs to RustSec;
neither a configured origin nor this offline gate authenticates a remote author.

The private snapshot retains the approved origin. Its receipt records that
identity, revision, tree, commit time, content digest, scanner digest and policy
digest. Rechecks detect changes during qualification. The receipt's
`database_url` and the fixed cargo-deny cache directory remain canonical RustSec
lookup identities because cargo-deny keys its default database that way. They
are not contacted: scanning remains `--offline --locked`, and policy database
overrides remain rejected. This does not configure credentials, network policy,
or any remote repository.

## License notices from any source host

`susi/upstream-notice/v1` continues to verify existing GitHub URL-bound records
offline. New records may use `susi/upstream-notice/v2`, which has no URL or host
requirement and proves the file's membership in its pinned Git commit locally.
Keep the original repository metadata, package identity, registry checksum,
revision, path in VCS, SHA-256, Git blob SHA-1 and license text. Do not rewrite
source identity merely because a copy came from a mirror.

V2 adds these proof fields:

- At record level, `git_commit_hex`: hex-encoded raw commit object payload,
  without Git's `commit <length>\0` header.
- On each `files` and `source_match` row, `git_tree_chain_hex`: an ordered
  array of hex-encoded raw tree object payloads, starting at that commit's
  root tree and containing one tree per slash-delimited `source_path` segment.
- URLs may be retained for human reference but are not used as evidence in v2.

Agents can obtain these bytes from an approved local object database using
ordinary `git cat-file commit <revision>` and `git cat-file tree <tree-id>`.
Preserve binary tree bytes exactly; do not decode or line-normalize them. Git's
object header is reconstructed for hashing. The commit hash must equal the
pinned revision, each tree hash must equal its parent entry, and the final
regular-file entry must name the supplied blob hash. Symlinks, submodules,
traversal, missing or surplus trees, malformed objects and altered bytes fail.
Proof metadata is bounded. No subprocess or network lookup is used by the
v2 verifier, and missing proofs never fall back to a plausible URL.

Package identity remains bound to Cargo.lock and available registry VCS
metadata. If repository or VCS metadata is absent, the existing requirement
for matching actual Rust source remains. A self-contained proof establishes
content consistency, not the authority of a revision supplied by the same
record. Independently verify upstream identity and the selected revision when
preparing any supplement; do not infer the license from a copied source file.
SPDX and nested-notice qualification are separate and are not relaxed here.

## MCP specifications and remaining migration work

The ordinary `susi mcp ledger` verifies the vendored pinned corpus locally.
For a separately supplied comparison corpus, use
`susi mcp ledger --drift --refetched <directory>`. Files are keyed by the
manifest's document paths; every byte count and SHA-256 must match the pin,
and a missing file fails closed. Supply independently acquired comparison
bytes: pointing this at the same vendored directory is an integrity check,
not evidence of a fresh upstream observation.

The optional live `--drift` path still defaults to GitHub's raw host and is not
rewritten by this change. Legacy shell branch handoff must never substitute
remote CI for the native gate; use `cargo local-workflow check` and `ship`.
This change does not qualify the legacy shell gate. No Actions workflow is
added or enabled. Repository protection, community
links, clean-cache provisioning, installation and upgrade against a non-GitHub
remote still need their own verification before claiming complete host
independence.
