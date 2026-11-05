# First-checkpoint compliance

## Same-repository rollout decision

The external-authority workflow below is a historical private proof. It is not
the proposed deployment. No external repository, installation token, or new
access grant is required for the following **advisory** same-repository design.

1. Separately review a bootstrap change to Susi main. Include the verifier,
   receipt schema, independently approved public key and primary fingerprint,
   a base-owned legacy policy runner, and a new read-only workflow. Keep the
   old push gate and unrelated checks. Preserve PR 1299's head and the v2
   signature as historical evidence.
2. Trigger on `pull_request_target` for open, sync, reopen and base retarget,
   or explicitly dispatch on protected main. Accept only main as the base.
   Use `permissions: {}` by default and grant the verification job only
   `contents: read` and `pull-requests: read`. Run on a fresh hosted runner,
   pin every action, check out only the trusted default-branch SHA and disable
   persisted credentials. Re-read main and fail stale invocations. Fetch PR
   commits into a separate bare object database. Do not check out or execute
   any PR source, build script, dependency configuration, action or cache.
   The existing T-task route runs only protected-base scripts. No secrets,
   signing key, write job or App publication step is needed.
3. Once main advances, **reissue against observed integration results**.
   The v2 verifier hard-pins the old base and expects it to be an ancestor of
   the head. A new signature over its old fields cannot fix either condition.
   In the reviewed successor, keep the PR/repository/branch allowlist and use
   the live protected base and immutable PR head. For a diverged base also
   fetch the actual proposed merge, verify both ordered parents, and record
   its tree. Stop on conflict, unavailable merge ref or unexpected parents.
   Capture both merge-base-to-head provenance and actual base-to-merge changed
   paths. Do not attribute bootstrap-only changes to the original worker.
4. Have the native integrator test that exact combined tree privately under
   the original gates, producing new nonempty results and scoped provenance.
   Independently review and sign a new versioned statement binding those
   receipts, repository, PR, base/head, merge parents, tree and exact paths.
   Deliver the signed bytes and receipts outside the tested tree (for example,
   a same-repository evidence ref published by the already authorized
   integrator). Treat the carrier as untrusted, bound the data read, pin the
   referenced evidence digests and verify the signature using only the
   public key from protected main. Committing the newly signed statement to
   main would change its own base. The previous 4,242-test receipt remains
   evidence about the previous tree only.
5. Surface the decision in the trusted Actions run summary, with the
   repository/PR/base/head/merge-tree, signer, signed digest, and gate scope.
   A main advance requires a fresh dispatch/reconciliation before merge.
   The integrator reads back the workflow path, event, run ID and attempt,
   trusted revision, conclusion and exact signed tuple, then rechecks live refs
   immediately before the existing serialized merge. Do not accept a green
   check name, a PR comment, or an arbitrary run URL as authority. Retain the
   other required checks and the separate merge authorization.

This repository is public and user-owned. Its observed main rules are deletion,
non-fast-forward and pull-request rules, with zero required approving reviews.
The workflow token defaults to read. `pull_request_target` job checks do not
by themselves establish a required head-SHA check. A check posted using
`GITHUB_TOKEN` also shares the GitHub Actions App identity with other
workflows. App selection plus a matching check name does not identify one
trusted workflow file. GitHub's required-workflow rules are configured at the
organization or enterprise level. Consequently this design provides a
verifiable workflow result for the existing integrator; it does not establish
server-enforced, workflow-specific protection against every other merge path.
No current permission or rule is changed by this design.

GitHub references: [secure pull request target runs](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target),
[required check behavior](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks),
[workflow ruleset limits](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/troubleshooting-rules).

## Historical proof

The protected verifier admits one native event: `intellibitz/susi#1299`, from
`codex/step1-qualified-0.21.43` to `main`, at the exact head and base compiled
into the verifier. The signer authorizes this checkpoint and its listed
provenance. Seven recorded trees carry native acceptance. Ten intermediate
merges contributed to tested batch trees. Linux unsafe, license and advisory
checks remain independent.

## Authority material

Use a protected automation repository and protected default branch controlled
independently of the Susi feature branch. Build the verifier from reviewed
source there. Place the following inputs in `authority/checkpoint-1299/` in
the trusted checkout or provision them from the integrator's protected store.

- `native-receipt` contains the exact bytes of the accepted integration
  receipt. `publication.json` contains the guarded publication and remote
  readback. The verifier checks both pinned SHA-256 digests.
- `statement.json` is the exact UTF-8 output, including its final LF, of the
  verifier's `statement` command. The reviewer independently checks its PR
  paths, worker sources, tested commits, gate results and current hosted refs.
- `statement.sig` is a detached binary OpenPGP signature over those bytes.
  `authority.gpg` is the binary public keyring supplied by the protected
  authority. Pin its independently approved uppercase primary fingerprint via
  the protected environment variable `SUSI_CHECKPOINT_PRIMARY_FINGERPRINT`.
  Keep signing keys outside the workflow.

```text
checkpoint-compliance statement SUBJECT NATIVE_RECEIPT PUBLICATION STATEMENT intellibitz/susi 1299 main codex/step1-qualified-0.21.43 BASE HEAD
checkpoint-compliance verify SUBJECT NATIVE_RECEIPT PUBLICATION STATEMENT intellibitz/susi 1299 main codex/step1-qualified-0.21.43 BASE HEAD SIGNATURE KEYRING PRIMARY_FINGERPRINT
```

## Hosted activation

Install the workflow on the authority's protected main and restrict the
`susi-checkpoint-authority` environment to that branch. Provision a read-only
subject token and a separate short-lived, repository-scoped installation token
for an independent GitHub App with checks write and PR/contents read access.
Keep the App's credentials unavailable to subject workflows. Configure the
required check by **name and issuing App**, and enforce current base at merge
time. Dispatch on PR open/synchronize/reopen and base advance. A manual
dispatch with the PR number uses the same live PR lookup. A base change
invalidates this fixed checkpoint even if the head stays the same.

The existing branch-push check named `Workflow Compliance` stays unchanged.
For any hosted migration, first prove the independently issued check for both
routes and retain every unrelated required gate. A same-named check from the
feature branch cannot substitute for the independently issued check. Existing
red check runs remain visible. Local integration has no dependency on this
adapter. No workflow installation, required-check change or GitHub status
publication is part of the private proof.

For PR 1299, deploy the reviewed verifier/workflow and their build dependencies
to the **separate authority repository's** protected main. Putting this
bootstrap onto Susi main first changes the signed PR base and requires a new
statement and independent signature. The detached evidence can live in the
authority's read-controlled bundle; its keyring is public material and its
signature is over the verified statement bytes. Authority admins select the
trusted fingerprint independently of the bundle and PR. Read back the check's
head SHA, conclusion and issuing App after the authorized dispatch.

## Private verification

Run `xtask/ci/test-checkpoint-compliance.ps1` with `-Verifier`, `-Subject`,
`-Receipt` and `-Publication`. Supply the actual Git history and readback
files. Alternatively set `SUSI_CHECKPOINT_VERIFIER`,
`SUSI_CHECKPOINT_AUTHORITY_DIR` and `SUSI_CHECKPOINT_PRIMARY_FINGERPRINT`.
This also checks the authentic signature, stale PR/base/head and a corrupted
signature packet. The test signs only with newly generated fixture keys in a
fresh temporary GnuPG home. It checks tampered input and signature refusals
and exercises the original T-task policy using a disposable claim snapshot.
