# Gate receipts

`local-workflow ship` retains the results of its gates in local Git
metadata. Receipts bind the exact source tree, commands, results, toolchain and
lockfile. `xtask verify-receipt <commit>` checks a retained receipt and
can re-run its commands. Missing history is not evidence of a passing gate.

## Publication and retention

A ship verifies integration head `H`, containing the fetched `origin/main` and
the selected source branches. The gate runs format, licenses, clippy, affected
test binaries, task acceptances and doctests on that exact tree. Before
publication it creates receipt-only child `R`, whose only change is the virtual
file `.agents/receipts/<H>.json`. The local ref `refs/susi/receipts/<H>` retains
`R`; `refs/susi/integration/current` names `H`.

Only `H` is pushed to `origin/main`. The worktree and published source gain no
receipt file. The existing Git object database stores the receipt outside
checked-out source, including its exact parent binding. A normal clone does not
fetch these private refs. Receipts committed to source before this change were
removed from the tree; each historical receipt commit remains readable by naming
that commit, and `--receipt <file>` accepts a separately supplied copy.

Publication is refused when the receipt cannot be built or verified, including
unreadable results, a missing required lockfile, a failed gate or an absent local
receipt ref. `ship --retry` rechecks the retained receipt and publishes `H`.
If local history is lost, run the gates again or supply the required proof to the
verifier; absence never qualifies a release. Required artifact provenance stays
with the artifact independently of optional operational history.

## The receipt (`susi-gate-receipt-v2`)

```json
{
  "schema": "susi-gate-receipt-v2",
  "digest": "<sha256 of the compact JSON of everything but this member>",
  "integration": {
    "commit": "<H>",
    "tree": "<H^{tree}>",
    "origin_main": "<the fetched origin/main integrated into H>",
    "merge_first_parent": "<H^1>"
  },
  "sources": [{"agent": "receipts", "commit": "<agent branch head>",
               "claim_base": "<reserved base>", "task": "T-RECEIPTS-1"}],
  "toolchain": {
    "host": "x86_64-pc-windows-msvc",
    "rustc": "<the full `rustc -vV` text>",
    "rustc_sha256": "<sha256 of that text>",
    "cargo_lock_sha256": "<sha256 of Cargo.lock at H>"
  },
  "selection": "packages:xtask,susi",
  "gates": [
    {
      "id": "workspace-tests",
      "argv": ["cargo", "test", "-p", "xtask", "--offline", "--locked",
               "--target", "x86_64-pc-windows-msvc"],
      "isolated_target_dir": true,
      "exit_code": 0,
      "elapsed_ms": 355700,
      "tests": {"passed": 133, "failed": 0, "ignored": 0},
      "log_sha256": "<sha256 of the retained gate log>",
      "binaries": [
        {"name": "local_workflow", "source": "unittests src/bin/local-workflow.rs",
         "counts": {"passed": 121, "failed": 0, "ignored": 0}}
      ]
    }
  ],
  "recorded_unix": 1794130000
}
```

Field by field:

- `gates[].argv` is the exact command the gate ran, in order (`format`,
  `licenses`, `clippy`, `workspace-tests`, one `task:<id>` per landed task,
  `doc-tests`), with one deliberate omission: the slot's `--target-dir <path>`
  is removed because it is an operator path. `isolated_target_dir: true`
  records that it was there, and a re-run supplies its own.
- `exit_code` and `elapsed_ms` are what the gate observed. Durations are
  informational; a re-run never compares them.
- `tests` is the gate's own total for a test gate (`cargo test`/`nextest`),
  `null` for the others. `binaries` lists every outer test harness the gate
  log shows (`name` is the executable's file stem without Cargo's metadata
  hash, `source` the `Running` header's source such as `unittests src/lib.rs`,
  `tests/cli.rs` or `doc-tests`) with its own pass/fail/ignored counts.
  Output of a fixture's nested `cargo test` inside a test is excluded by the
  executable path, so only the gate's own binaries are listed. Cargo prints
  that path relative to the gate's working directory when the target
  directory is inside it (a serial `cargo test` gate, such as a task's
  acceptance command) and absolute otherwise; both forms are read. The
  legacy receipt format may contain totals without a per-harness list.
- `log_sha256` is the digest of the gate log retained in the gate host's Git
  object store (`refs/susi/provenance/receipts/*` names the blobs); the
  receipt carries the digest, not the log.
- `toolchain.rustc` is the pinned compiler's `rustc -vV` output
  (`rust-toolchain.toml` pins it for every agent); its SHA-256 and the host
  triple let a verifier tell "cannot reproduce on this host" from "the result
  differs".
- `cargo_lock_sha256` binds the dependency graph. `digest` binds the body:
  `serde_json` orders object members by key, so the compact encoding of the
  receipt minus `digest` is canonical and any edit is detected.
- The receipt names no path, user or machine. Agents appear by their claim
  name (also the branch and task-record name); the host appears as its triple.

The commercial gate (`susi-commercial-apps`, same tool) keeps no native gate
plan, so its receipts carry each command's exit code and duration from the
evidence lines but no test totals or harness list.

## Re-running a receipt

```text
cargo fetch --locked            # once; the gates run --offline
xtask verify-receipt <H>   # retained receipt in this clone
xtask verify-receipt <H> --receipt <file>  # separately supplied receipt
```

`<H>` is the verified merge (the receipt's `integration.commit`); the receipt
commit `R` or any abbreviation works too when that object is available. The
tool checks an explicit file first, then the retained local receipt ref, then
legacy source copies at the working tree or `HEAD`, or `R`'s tree when given
`R`. A damaged local ref is refused instead of silently using a legacy copy.
Legacy v1 receipts retain their original first-parent semantics.
A supplied JSON file can reproduce gate observations, but without its original
receipt commit the structural check remains skipped and the command exits
nonzero. Transfer the corresponding private ref explicitly through Git when
complete historical verification is required; an ordinary source clone never
claims to contain that proof.

The tool reports one line per check and exits nonzero on a mismatch or skipped check:

1. Static checks against the clone: schema and digest; `integration.commit`
   and `integration.tree` are `<H>` and its tree; the observed integration
   base and first parent match the recorded schema; `cargo_lock_sha256`
   matches `git show H:Cargo.lock`; a retained receipt commit or legacy source
   addition is `H`'s single-parent child changing only the receipt file;
   every recorded gate exited 0. A supplied file without its original receipt
   commit reports that structural check as unavailable, never as proved.
2. Toolchain: the local `rustc -vV` (run inside the scratch worktree, so the
   pinned toolchain applies) must have the recorded SHA-256 and host. When it
   does not, the gates are **skipped**, not failed: the tool cannot say
   whether a difference would be the code or the compiler. `--ignore-toolchain`
   runs them anyway and the report says so.
3. Re-runs: a detached scratch worktree of `H` is created under
   `target/verify-receipt/<H12>-tree` with its own target directory
   (`<H12>-target`, kept between runs so a second verification is warm) and
   a fresh, empty home (`HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`,
   `XDG_*`, `SUSI_HOME` all point into it; `SUSI_*` and credential-shaped
   variables are removed; `CARGO_HOME`/`RUSTUP_HOME` are kept). Each gate's
   `argv` runs there in order, its log lands in `<H12>-logs/<id>.log`, and the
   tool compares the exit code, the test totals and the per-harness counts
   with the receipt. A gate that ran with its own target directory gets the
   scratch one, inserted before any `--` so it reaches Cargo and not the
   tool behind it (clippy's `-- -D warnings`). `--only <gate-id>`
   (repeatable) limits the re-run, `--static-only` skips it, `--keep` leaves
   the worktree and home in place, `--scratch <dir>` moves the scratch root.
   The scratch worktree is checked out with `core.autocrlf=false` and
   `core.eol=lf`, as the gate checks out its candidate, so both test the
   committed bytes whatever the host's Git configuration says (Git for
   Windows sets `core.autocrlf=true` system-wide; with the home moved, a
   user's own `false` is not read, and a CRLF checkout fails the GMCP
   ledger's pinned source hashes). It also uses `core.longpaths=true`; on Windows
   a clone at a deep path should still pass a short `--scratch` (for example
   `C:\vr`), because the build and the test binaries under a long target path
   were not exercised beyond the checkout. One rule of comparison: a
   narrowed test gate builds its binaries with `--no-run` and runs them
   through the concurrent runner, so its receipt lists no doctests (the
   separate `doc-tests` gate covers them); a plain re-run of the same
   `cargo test` argv also prints a trailing doctest phase, which the tool
   drops from the observed harnesses and totals when the receipt recorded
   none. A receipt that did record doctests (the serial whole-workspace
   gate) is compared in full.

Prerequisites: the pinned toolchain (rustup installs it from
`rust-toolchain.toml`), `cargo-deny` for the `licenses` gate, and a populated
registry cache (`cargo fetch --locked`) because every gate runs `--offline`.
Expect 20-40 minutes for a receipt whose test selection covers a few crates;
the whole-workspace selection takes about as long as the gate did.

A mismatch is reported as such. Missing tools or failed tests remain failures;
a different toolchain is reported as a skip unless explicitly overridden.

## For the gate host

Nothing changes in how a landing is run: `local-workflow ship <agent>`
does everything above. Two details worth knowing:

- The receipt is built in `record_acceptance`, from the native gate plan
  (`native-gates.json`: ids, argv, status, duration, counts and the retained
  log blobs) when the gate kept one, else from the evidence lines. It is
  committed with Git plumbing alone (temporary index, `write-tree`,
  `commit-tree`), so no worktree is touched. Its retention ref is local;
  the published tree is exactly `H`'s tree.
- `ship --retry` validates the retained receipt before pushing `H`. A head
  without that local proof cannot be retried; run `ship` again so its gates
  produce a new receipt.

Unit tests for the writer, the parser and the verifier are in
`tools/core-xtask/src/gate_receipt.rs`; the ship tests in
`tools/core-xtask/src/local_workflow/tests.rs` check that publication preserves the
verified source tree, local proof is required, and explicitly supplied receipts
remain verifiable after an ordinary clone.

## Complete workspace binary execution (staged)

The experimental `workspace-binaries` path is **not available for production
activation, receipt publication, or full public replay yet**. The pinned stable
rustdoc rejects `--test` with `--emit=dep-info`; compile-only doctest discovery
also requires unstable options. The current Git-visible input snapshot cannot
account for ignored files consumed by doctests. An explicit request fails
closed rather than pretending that snapshot is complete. No unstable flags or
source-code include scanner are used. The default remains unchanged. Linux
continues to use serial Cargo; the runner does not yet reproduce its dynamic
library runtime contract. `serial` and `nextest` retain their existing meanings.

This mode records the command that actually compiles the tests, including
`--no-run --message-format=json`, and an explicit
`susi/cargo-test-binaries/v1` execution descriptor. Compilation alone is never
accepted as executed tests. Cargo's complete strict JSON inventory is sealed
before launch, each exact executable is accounted for, and matching companion
doctests must pass. A binary's nested Cargo uses its private home target while
runtime library lookup retains the outer target. Unsupported build-script
library search paths or reserved runtime environment overrides fail closed;
serial Cargo remains available. This mode emits no serial
`workspace-test-pass-v1` proof and changes no fresh production qualification.

Native capture retains immutable raw Cargo output, metadata, execution plan,
and results in the local Git object store. Its private retention refs do not
travel with an ordinary publication/fetch. Historical validation of those
original artifact identities requires the retained objects, but not the old
worktree or target directory. The public receipt intentionally carries only a
path-free typed descriptor, counts, and the original selection. The experimental
replay plumbing rebuilds a fresh inventory from source in an isolated checkout,
executes it, runs the exact companion doctests, and compares observations. It
does not claim to recover or independently authenticate old private artifacts.
Selecting this mode with `--only` requires selecting `doc-tests` too. A missing
or unknown descriptor is rejected before a build-only command can run.

Absent descriptors preserve historical serial/nextest replay unchanged.
Inclusive timing covers input and artifact hashing and final checks; no
throughput improvement or production acceptance is established by these new
fixtures alone.
