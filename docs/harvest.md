# Harvest: `local-harvest`

`local-harvest` is the tooling for [CEO Mandate 74, section 5](ceo-mandate-74.md#5-harvest-the-open-source-world-is-the-oyster).
Use it to take Apache-2.0 code (plus MIT, BSD-2-Clause and BSD-3-Clause, per
Mandate 68) from any public Git repository on any host and keep every license
obligation. It is native Rust (`xtask/src/harvest.rs`) and depends only on the
`git` executable. It uses no hosting API and no network beyond `git fetch`.

It runs in whatever Git checkout is the current directory, so
`susi-commercial-apps` runs the same binary.

```sh
cargo run --offline --locked -p xtask --bin local-harvest -- <command> [--json]
```

Every command prints text by default. With `--json` it prints machine output
on stdout. A non-zero exit code means refusal or a failed check.

## Safety model

- Upstream code is untrusted. It is fetched into a local quarantine ref and
  read only as Git objects (`git ls-tree`, `git cat-file`). It is never checked
  out, built or executed during intake, scan or import.
- Quarantine refs are local only: `refs/susi/upstream/<slug>/<full-sha>`,
  created with a create-only `git update-ref`. Never push them.
- The source URL is recorded at intake in
  `<git-common-dir>/susi-harvest/<slug>/<sha>.url`. The common Git directory is
  shared by all worktrees of a clone and never leaves the host. `import` copies
  the URL into the manifest. If the record is missing (for example on another
  clone), re-run `intake` or pass `--url`.

## Commands

```sh
# 1. Quarantine fetch, pinned to one commit (a ref name is resolved to its SHA)
local-harvest intake --url https://codeberg.org/acme/widgets.git --rev v1.4.0 --slug widgets
#   pinned 3f2c...e9 / quarantine ref: refs/susi/upstream/widgets/3f2c...e9

# 2. Per-file license verification at that commit (files or directories)
local-harvest scan --slug widgets --rev 3f2c...e9 --path src/codec --path NOTICE

# 3. Import approved files, write the manifest, LICENSES and notices (no commit)
local-harvest import --slug widgets --rev 3f2c...e9 \
  --map src/codec=crates/susi-codec/src/upstream \
  --id H-CLAUDE-1 --task T-CLAUDE-7 --feature codec [--mode transplant|port] \
  [--summary "trimmed to the decoder"] [--agent claude] [--overwrite]

# 4. Regenerate THIRD_PARTY_NOTICES from all manifests, or check it
local-harvest notices
local-harvest notices --check

# 5. Audit every manifest against Apache-2.0 section 4, plus commit trailers
local-harvest audit --range origin/main..HEAD
```

**scan** decides one license per file:

1. An `SPDX-License-Identifier:` line in the first 30 lines wins.
2. Otherwise the tool uses the nearest license file found by walking up to the
   root (`LICENSE`, `LICENSE.txt`, `LICENSE.md`, `LICENSE-APACHE`,
   `LICENSE-MIT`, `COPYING`). It classifies that file by its text as
   Apache-2.0, MIT, BSD-3-Clause, BSD-2-Clause or unknown. A directory with
   exactly `LICENSE-APACHE` and `LICENSE-MIT` counts as `Apache-2.0 OR MIT`.
   Several different licenses in one directory are combined with `AND`.

The allowed licenses are `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `MIT`,
`BSD-2-Clause` and `BSD-3-Clause`. An `OR` expression passes when one
alternative is allowed, and the tool picks Apache-2.0 when it is offered.

The scan rejects:

- every other license, and files with no license at all;
- symlinks and submodules;
- any file whose text mentions the GNU GPL, LGPL or AGPL, the Mozilla Public
  License, SSPL, the Business Source License or the Commons Clause.

Scan also gathers the root `NOTICE*` files and the nearest-directory `NOTICE*`
files, and the `Copyright` lines in each file and its license file. It exits
non-zero if any file is rejected.

**import** runs the same scan and refuses everything if a single file is
rejected. Destination paths must be confined relative paths: no `..`, no `.git`
and no drive letters. Existing files are never overwritten without
`--overwrite`.

Import then does the following:

- Writes the upstream blobs byte for byte.
- Writes `.agents/harvest/<id>.json`.
- Creates `LICENSES/Apache-2.0.txt` from the upstream license text when that
  file is missing.
- Regenerates `THIRD_PARTY_NOTICES`.
- Prints the commit trailers to use: `Harvest:`, one `Origin: <url>@<sha>:<path>`
  per map, `Origin-License:`, `Task:` and `Feature:`.

In `--mode port`, each map is one upstream file to one destination file.
Nothing upstream is written. You write the Rust port yourself, keep the
copyright lines and add a `Modified by` notice. A port is a derivative work.

## Manifest `susi-harvest-manifest-v1`

| Field | Meaning |
| --- | --- |
| `schema`, `id`, `task`, `feature` | `susi-harvest-manifest-v1`, `H-<AGENT>-<n>` (must match the file name), `T-<AGENT>-<n>`, feature id or `null` |
| `source_url`, `source_commit`, `quarantine_ref` | Where the code came from and the exact pinned commit |
| `mode` | `transplant` or `port` |
| `license` | The chosen SPDX license. Different per-file licenses are joined with `AND` |
| `license_evidence[]` | `{path, kind: license\|notice, spdx, blob_sha1, sha256}` for every license and NOTICE file used |
| `license_texts[]` | `{path, spdx, text}` for MIT/BSD license files, which must be reproduced |
| `notice_text` | Upstream NOTICE contents, each under a `--- <path> ---` header |
| `copyright_lines` | Copyright lines from the harvested files and their license files, deduplicated and sorted |
| `modifications` | Free-text summary (`--summary`) |
| `files[]` | `{source_path, dest_path, source_blob_sha1, spdx, copyright_lines}` |
| `harvested_by`, `harvested_unix` | `--agent` or `git config user.name`, and the import time |

`THIRD_PARTY_NOTICES` is generated deterministically from the manifests, sorted
by id. Each manifest gets a section with its id, `url@commit`, license, files,
copyright lines, NOTICE text and MIT/BSD license texts. Never edit it by hand.

## What `audit` checks

| Apache-2.0 section 4 | Check |
| --- | --- |
| (a) give recipients the license | `LICENSES/Apache-2.0.txt` exists and is the Apache 2.0 text whenever any manifest license includes Apache-2.0 |
| (b) modified files carry a prominent notice | A destination file that differs from its upstream blob must contain a line matching `modified by` (case-insensitive), e.g. `// Modified by Susi contributors, 2026-11-11: <summary>` |
| (c) keep copyright and attribution notices | Every copyright line recorded for a source file still appears in its destination file |
| (d) carry the NOTICE attributions | `THIRD_PARTY_NOTICES` exists and is exactly what `notices` would generate |

`audit` also checks:

- every manifest's schema, ids and hashes;
- that every license is allowed;
- that every destination file exists.

When the upstream commit is present locally, `audit` also checks every recorded
blob id and evidence `sha256` against it. Without the commit, `audit` hashes
each destination as a Git blob (`git hash-object`, with and without clean
filters) and compares the result with the recorded `source_blob_sha1`. That
works offline and tells exactly whether a file was modified.

`--range A..B` reads every commit's `Harvest:` trailers with
`git log --format=%(trailers:key=Harvest,valueonly)`. Each id must have a
manifest.

## Not covered (be honest about it)

- License detection is heuristic text and SPDX matching, not legal review.
  Custom, dual or unusual licenses, and license grants in a README, are not
  understood. Anything unrecognized is rejected, so if you are in doubt, do
  not take the code.
- The reject-phrase check is a text search. It can reject a permissive file
  that merely mentions the GPL. It cannot see license terms that are only
  implied.
- Harvesting the code without the history, or porting it from another
  language, is not detected automatically. Use `--mode port`, labels and
  trailers. Audit only checks files listed in manifests.
- Patent clauses, trademarks, logos and product names (Apache-2.0 section 6)
  are not reviewed automatically.
- An MIT/BSD file that carries only an SPDX header, with no license file,
  gets no license text in `THIRD_PARTY_NOTICES`. Add it by hand to the
  destination if the upstream file did not include it.
- `local-release qualify` (the installed-production package path) runs the same
  audit in-process on the exact integrated candidate, over its whole history,
  before any build. It fails closed and records `harvest-audit=` in the
  manifest-bound `gates.txt`. `susi-commercial qualify-release --host-only`
  runs this tool on its source checkout and refuses the release if the audit
  fails or the tool is missing.
- `release-qualify` runs `local-harvest audit` automatically before any build
  or test gate, records `harvest_audit` in `releases/qualification/<tag>.json`,
  and `push-policy` (`verify_tag`) requires a passing `harvest_audit` whenever
  the qualified tree contains `.agents/harvest/*.json` manifests. That enforces
  recorded manifests and `Harvest:` commit trailers, not legal review of
  unrecorded ports.
