use super::*;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(in crate::local_workflow) fn require_ship_ref(git_ref: &str) -> Result<()> {
    if git_ref != "refs/heads/main" {
        return Err("ship pushes only verified fast-forwards to refs/heads/main".into());
    }
    Ok(())
}

/// The git remote a ship fetches `main` from and fast-forwards. Plain git
/// only: GitHub is storage (CEO mandate 2026-11-09), so a landing posts
/// nothing to the host; its receipt commit is the record.
pub(in crate::local_workflow) trait Remote {
    fn fetch_main(&mut self, root: &Path) -> Result<String>;
    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()>;
}

pub(in crate::local_workflow) struct GitRemote;

pub(in crate::local_workflow) fn strip_ansi(text: &str) -> String {
    common::strip_ansi(text)
}

pub(in crate::local_workflow) fn sync_local_main(root: &Path, sha: &str) {
    let _ = git(root, &["update-ref", "refs/remotes/origin/main", sha]);
    if let Ok(common) = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        if let Some(primary) = Path::new(&common).parent() {
            if git(primary, &["branch", "--show-current"]).ok().as_deref() == Some("main") {
                // Moving a checked-out branch ref without its files would make
                // the checkout look like it reverts the shipped work. Untracked
                // files are fine: a fast-forward refuses to overwrite them.
                let clean = git(primary, &["status", "--porcelain", "--untracked-files=no"])
                    .is_ok_and(|status| status.is_empty());
                let merged = clean
                    && git(
                        primary,
                        &[
                            "-c",
                            "core.hooksPath=",
                            "merge",
                            "--ff-only",
                            "--quiet",
                            sha,
                        ],
                    )
                    .is_ok();
                if !merged {
                    eprintln!(
                        "local main in {} was not fast-forwarded (uncommitted changes); run `git merge --ff-only origin/main` there",
                        primary.display()
                    );
                }
                return;
            }
        }
    }
    let checked_out = git(root, &["worktree", "list", "--porcelain"])
        .is_ok_and(|list| list.lines().any(|line| line == "branch refs/heads/main"));
    if !checked_out {
        let _ = git(root, &["update-ref", "refs/heads/main", sha]);
    }
}

impl Remote for GitRemote {
    fn fetch_main(&mut self, root: &Path) -> Result<String> {
        git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "fetch",
                "--quiet",
                "--no-tags",
                "--recurse-submodules=no",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        )?;
        let oid = git(root, &["rev-parse", "origin/main"])?;
        if !valid_oid(&oid) {
            return Err("origin/main is not a commit oid".into());
        }
        Ok(oid)
    }

    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()> {
        require_ship_ref(git_ref)?;
        if !valid_oid(sha) {
            return Err("ship push requires a commit oid".into());
        }
        git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "push",
                "--quiet",
                "origin",
                &format!("{sha}:{git_ref}"),
            ],
        )?;
        sync_local_main(root, sha);
        Ok(())
    }
}

/// What a ship published: `head` is the commit now on origin/main (the
/// receipt commit), `merge` the verified integration head it sits on and
/// whose receipt it carries.
#[derive(Debug)]
pub(in crate::local_workflow) struct ShipReport {
    pub(in crate::local_workflow) head: String,
    pub(in crate::local_workflow) merge: String,
}

pub(in crate::local_workflow) fn gate_evidence<'a>(
    evidence: &'a [String],
    subcommand: &str,
) -> Result<&'a str> {
    evidence
        .iter()
        .find(|line| {
            let Some((command, status)) = line.split_once(" => ") else {
                return false;
            };
            status.starts_with("exit code: 0;")
                && command.split_whitespace().nth(1) == Some(subcommand)
        })
        .map(String::as_str)
        .ok_or_else(|| format!("missing passing {subcommand} evidence").into())
}

pub(in crate::local_workflow) fn require_ship_evidence(evidence: &[String]) -> Result<()> {
    if evidence.is_empty() {
        return Err("verification returned no evidence".into());
    }
    // Gate lines are `cargo ... => exit code: N; ...`. Other evidence (sysroot,
    // test proof, gate selection) describes the run and carries no status.
    for line in evidence.iter().filter(|line| line.starts_with("cargo ")) {
        match line.split_once(" => ") {
            Some((_, status)) if status.starts_with("exit code: 0;") => {}
            _ => return Err(format!("refusing to publish a failed gate: {line}").into()),
        }
    }
    for subcommand in ["fmt", "deny", "clippy"] {
        gate_evidence(evidence, subcommand)?;
    }
    if gate_evidence(evidence, "nextest").is_err() {
        gate_evidence(evidence, "test")?;
    }
    Ok(())
}

pub(in crate::local_workflow) struct SourceInput {
    pub(in crate::local_workflow) claim: Claim,
    pub(in crate::local_workflow) source: String,
    pub(in crate::local_workflow) bytes: Vec<u8>,
    pub(in crate::local_workflow) path: PathBuf,
}

pub(in crate::local_workflow) fn ship_scope_base(
    root: &Path,
    claim: &Claim,
    source: &str,
    publish_base: &str,
) -> Result<String> {
    let base = match scope_base(root, claim, source) {
        Ok(base) => base,
        Err(error) => {
            let on_publish_base =
                claim.base == publish_base || is_ancestor(root, &claim.base, publish_base)?;
            // Falling back to the claim base exempts nothing: the whole
            // claim.base..source diff stays subject to the claimed scopes.
            let message = error.to_string();
            let recoverable = message.contains("accepted history does not descend")
                || message.contains("accepted integration has no matching durable");
            if recoverable && is_ancestor(root, &claim.base, source)? && on_publish_base {
                claim.base.clone()
            } else {
                return Err(error);
            }
        }
    };
    // History the source shares with the publish base is already on
    // origin/main: ship merges onto it and gates the result, so it is not
    // this worker's change to answer for. When origin/main advanced past the
    // accepted integration without a receipt (a push around the gate, seen
    // 2026-11-07: 170 commits) and the agent merged it, measuring from the
    // accepted integration would blame every one of those files on the
    // agent. The scope base moves up to the shared ancestor, never past
    // anything the agent added itself.
    let shared = git(root, &["merge-base", "--all", source, publish_base])?;
    let shared: Vec<_> = shared.lines().collect();
    if let [shared] = shared.as_slice() {
        if *shared != base && is_ancestor(root, &base, shared)? {
            return Ok((*shared).to_owned());
        }
    }
    Ok(base)
}

/// The expected directory belongs to the lock held by this operation. Recheck
/// at each batch boundary; never carry this observation across a gate silently.
pub(in crate::local_workflow) fn require_workflow(root: &Path, expected: &Path) -> Result<()> {
    if state(root)? != expected {
        return Err("repository workflow directory changed during operation".into());
    }
    Ok(())
}

pub(in crate::local_workflow) fn check_ship_source(
    root: &Path,
    workflow: &Path,
    claim: &Claim,
    publish_base: &str,
) -> Result<String> {
    if state(&claim.worktree)? != workflow
        || git(&claim.worktree, &["branch", "--show-current"])?
            != format!("local-agent/{}", claim.name)
    {
        return Err("claim repository or branch does not match".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("integration requires a clean committed agent worktree".into());
    }
    let source = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if !is_ancestor(root, &claim.base, &source)? {
        return Err("source no longer descends from its reserved base".into());
    }
    let scope_base = ship_scope_base(root, claim, &source, publish_base)?;
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &scope_base,
            &source,
        ])
        .output()
        .map_err(|error| format!("git diff in {}: {error}", root.display()))?;
    if !out.status.success() {
        return Err("cannot inspect committed scope".into());
    }
    for path in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = scope(std::str::from_utf8(path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &path)) {
            return Err(format!("committed file is outside reserved scope: {path}").into());
        }
    }
    Ok(source)
}

pub(in crate::local_workflow) fn load_sources(
    root: &Path,
    workflow: &Path,
    claimants: &[&str],
    publish_base: &str,
) -> Result<Vec<SourceInput>> {
    if claimants.is_empty() {
        return Err("at least one claimant is required".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for claimant in claimants {
        name(claimant)?;
        if !unique.insert(*claimant) {
            return Err("duplicate integration claimant".into());
        }
    }
    require_workflow(root, workflow)?;
    let _claims = Lock::acquire(workflow)?;
    let inputs = claimants
        .iter()
        .map(|claimant| {
            let path = workflow.join("claims").join(format!("{claimant}.claim"));
            let bytes = fs::read(&path)?;
            let claim = read_local_claim(&path)?;
            let source = check_ship_source(root, workflow, &claim, publish_base)?;
            Ok(SourceInput {
                claim,
                source,
                bytes,
                path,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    require_workflow(root, workflow)?;
    Ok(inputs)
}

/// One stable candidate worktree per repository, beside its primary checkout
/// (`<repo>-verify-candidate`). Sibling repositories never share it, and the
/// warm integration target keeps the same paths from run to run.
pub(in crate::local_workflow) fn candidate_path(root: &Path) -> Result<PathBuf> {
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let primary = common.parent().ok_or("git common dir has no parent")?;
    let name = primary
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("primary checkout has no name")?;
    Ok(primary
        .parent()
        .ok_or("primary checkout has no parent")?
        .join(format!("{name}-verify-candidate")))
}

/// Where `candidate`'s warm target rests between runs, in order of
/// preference. A slot candidate (`<repo>-verify-candidate-<n>`) has its own
/// `slots/<n>/target`; when that does not exist yet it adopts the single
/// legacy `integration-target` (registry dependencies stay fresh; workspace
/// crates rebuild once for the new path). The legacy candidate
/// (`integrate`) uses only the legacy target.
pub(in crate::local_workflow) fn warm_targets(workflow: &Path, candidate: &Path) -> Vec<PathBuf> {
    let legacy = workflow.join("integration-target");
    let slot = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once("-verify-candidate-"))
        .map(|(_, slot)| slot)
        .filter(|slot| !slot.is_empty() && slot.bytes().all(|byte| byte.is_ascii_digit()));
    match slot {
        Some(slot) => vec![workflow.join("slots").join(slot).join("target"), legacy],
        None => vec![legacy],
    }
}

/// Move a warm target into `candidate` (a rename: atomic, so two candidates
/// never take the same one) and return the candidate's target directory.
pub(in crate::local_workflow) fn restore_candidate_target(candidate: &Path) -> PathBuf {
    let target = candidate.join("target");
    if !target.exists() {
        if let Ok(workflow) = state(candidate) {
            for warm in warm_targets(&workflow, candidate) {
                if warm.is_dir() && fs::rename(&warm, &target).is_ok() {
                    break;
                }
            }
        }
    }
    target
}

pub(in crate::local_workflow) fn stash_candidate_target(root: &Path, candidate: &Path) {
    let target = candidate.join("target");
    if target.is_dir() {
        if let Ok(workflow) = state(root) {
            if let Some(warm) = warm_targets(&workflow, candidate).into_iter().next() {
                if !warm.exists() {
                    if let Some(parent) = warm.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::rename(&target, &warm);
                }
            }
        }
    }
}

/// Remove a previous run's `candidate` worktree, or say exactly why it
/// cannot be removed. On Windows a target directory whose files a leftover
/// process still holds open is only partly deleted; `worktree add` into the
/// remains then fails with a misleading error, so the real removal error is
/// returned instead of ignored.
pub(in crate::local_workflow) fn clear_candidate(root: &Path, candidate: &Path) -> Result<()> {
    if candidate.exists() {
        remove_candidate(root, candidate);
    }
    if candidate.exists() {
        if let Err(error) = fs::remove_dir_all(candidate) {
            if candidate.exists() {
                return Err(format!(
                    "cannot remove the previous candidate {}: {error}",
                    candidate.display()
                )
                .into());
            }
        }
    }
    // Only this candidate's record: a bare `git worktree prune` also deletes
    // the records of live worktrees registered from the Windows boot.
    worktree_admin::forget(root, candidate)?;
    Ok(())
}

pub(in crate::local_workflow) fn remove_candidate(root: &Path, candidate: &Path) {
    stash_candidate_target(root, candidate);
    if let Ok(path) = text(candidate) {
        let _ = git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "core.longpaths=true",
                "worktree",
                "remove",
                "--force",
                path,
            ],
        );
    }
}

/// Recreate the slot's `candidate` worktree at `base` and merge every source
/// into it, in order. Both check files out as committed
/// (`gate_receipt::CHECKOUT_AS_COMMITTED`), so the gate tests the bytes its
/// receipt names whatever the host's `core.autocrlf` says.
pub(in crate::local_workflow) fn merge_onto(
    root: &Path,
    candidate: &Path,
    base: &str,
    inputs: &[SourceInput],
) -> Result<()> {
    if !valid_oid(base) {
        return Err("ship base is not a commit oid".into());
    }
    clear_candidate(root, candidate)?;
    let [c1, autocrlf, c2, eol] = gate_receipt::CHECKOUT_AS_COMMITTED;
    git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            c1,
            autocrlf,
            c2,
            eol,
            "worktree",
            "add",
            "--detach",
            text(candidate)?,
            base,
        ],
    )?;
    for input in inputs {
        if let Err(error) = git(
            candidate,
            &[
                "-c",
                "core.hooksPath=",
                c1,
                autocrlf,
                c2,
                eol,
                "-c",
                "user.name=Susi Local Integration",
                "-c",
                "user.email=susi-local@localhost",
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-ff",
                "--no-edit",
                &input.source,
            ],
        ) {
            remove_candidate(root, candidate);
            return Err(error);
        }
    }
    Ok(())
}

pub(in crate::local_workflow) struct PublicationHead<'a> {
    pub(in crate::local_workflow) head: &'a str,
    pub(in crate::local_workflow) observed_base: &'a str,
}

pub(in crate::local_workflow) fn record_acceptance(
    root: &Path,
    attempt: &Path,
    inputs: &[SourceInput],
    publication: PublicationHead<'_>,
    evidence: &[String],
) -> Result<()> {
    let PublicationHead {
        head,
        observed_base: publish_base,
    } = publication;
    // This is the exact final fetch checked under the publication lock, not
    // HEAD^1: after a multi-source batch that parent is an intermediate merge.
    if !valid_oid(publish_base) || !is_ancestor(root, publish_base, head)? {
        return Err("fetched publication base is not contained in the verified head".into());
    }
    let native_gates = if attempt.join("native-gates.json").is_file() {
        Some(serde_json::from_slice::<serde_json::Value>(&fs::read(
            attempt.join("native-gates.json"),
        )?)?)
    } else {
        None
    };
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let previous = integration_head(root)?;
    // The receipt's `previous` names the accepted history this integration
    // builds on. A displaced unrelated ref is not that history: record the
    // publish base instead, or the receipt can never verify and every later
    // ship fails at load. The ref transaction still compares against the
    // actual old value.
    let mut chain_previous = previous.clone();
    if let Some(previous) = &previous {
        if !is_ancestor(root, previous, head)? {
            git(
                root,
                &["update-ref", "refs/susi/integration/displaced", previous],
            )?;
            eprintln!(
                "preserved unrelated integration ref {previous} at refs/susi/integration/displaced"
            );
            chain_previous = Some(publish_base.to_owned());
        } else if previous.as_str() != publish_base && is_ancestor(root, previous, publish_base)? {
            // The local ref is behind origin/main (main advanced through ships
            // made elsewhere). The history this integration builds on is the
            // publish base it was merged onto, not the stale ref, which can be
            // older than the claim base and would make the receipt unverifiable.
            chain_previous = Some(publish_base.to_owned());
        }
    }
    let zero = "0".repeat(head.len());
    let mut transaction = format!(
        "start\nupdate {INTEGRATION_REF} {head} {}\n",
        previous.as_deref().unwrap_or(&zero)
    );
    for input in inputs {
        let receipt = format!(
            "susi-local-integration-v1\nagent={}\nsource={}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n",
            input.claim.name,
            input.source,
            input.claim.base,
            chain_previous.as_deref().unwrap_or("none"),
            evidence.join("\n")
        );
        let oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
        let receipt_ref = format!(
            "refs/susi/integration/receipts/{}/{}",
            input.claim.name, input.source
        );
        transaction.push_str(&format!("create {receipt_ref} {oid}\n"));
        if let Some(gates) = &native_gates {
            let scope_base = timed("acceptance: scope base", || {
                ship_scope_base(root, &input.claim, &input.source, publish_base)
            })?;
            let native = timed("acceptance: provenance receipt", || {
                native_provenance::receipt(
                    root,
                    native_provenance::IntegrationSource {
                        worker: &input.claim.name,
                        claim_bytes: &input.bytes,
                        source: &input.source,
                        scope_base: &scope_base,
                        previous: chain_previous.as_deref(),
                        accepted: head,
                    },
                    gates,
                )
            })?;
            let native_oid = timed("acceptance: store receipt", || {
                native_provenance::store_blob(root, &serde_json::to_vec(&native)?)
            })?;
            let native_ref = format!(
                "refs/susi/provenance/receipts/{}/{}",
                input.claim.name, input.source
            );
            transaction.push_str(&format!("create {native_ref} {native_oid}\n"));
        }
    }
    // The public gate receipt: `.agents/receipts/<head>.json` in a
    // receipt-only commit on top of the verified head, recorded in the same
    // ref transaction. `publish_head` pushes that commit; without it nothing
    // is published.
    let sources: Vec<serde_json::Value> = inputs
        .iter()
        .map(|input| {
            let task = native_provenance::task_for_claim(
                root,
                &input.source,
                &input.claim.name,
                &input.claim.scopes,
            )
            .ok()
            .map(|task| task.id);
            serde_json::json!({
                "agent": input.claim.name,
                "commit": input.source,
                "claim_base": input.claim.base,
                "task": task,
            })
        })
        .collect();
    let receipt = timed("acceptance: gate receipt", || {
        gate_receipt::build(&gate_receipt::Landing {
            root,
            head,
            origin_main: publish_base,
            sources: sources.clone(),
            native_gates: native_gates.as_ref(),
            evidence,
        })
    })
    .map_err(|error| format!("gate receipt: {error}"))?;
    let receipt_commit = timed("acceptance: receipt commit", || {
        gate_receipt::commit_receipt(
            root,
            head,
            &receipt,
            &gate_receipt::commit_message(head, &sources),
            &attempt.join("receipt-index"),
        )
    })
    .map_err(|error| format!("gate receipt commit: {error}"))?;
    transaction.push_str(&format!(
        "update {} {receipt_commit}\n",
        gate_receipt::ref_for(head)
    ));
    transaction.push_str("prepare\ncommit\n");
    timed("acceptance: ref transaction", || {
        git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())
    })?;
    for input in inputs {
        fs::remove_file(&input.path)?;
    }
    Ok(())
}

/// Run `verify` on the slot's merged candidate. `selection_base` is what the
/// gate's affected selection is computed against: the publish base on a first
/// run, the previously verified head on a re-gate.
pub(in crate::local_workflow) fn verify_merged<F>(
    slot: &Slot,
    selection_base: &str,
    inputs: &[SourceInput],
    verify: &mut F,
) -> Result<(String, Vec<String>)>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let candidate = &slot.candidate;
    let attempt = &slot.attempt;
    fs::create_dir_all(attempt)?;
    // The native provenance receipt carries the final run's gates; an earlier
    // run of this ship (before a re-gate) stays beside it for inspection.
    if attempt.join("native-gates.json").is_file() {
        let selection = fs::read_to_string(attempt.join("selection-base")).unwrap_or_default();
        let _ = fs::rename(
            attempt.join("native-gates.json"),
            attempt.join(format!("native-gates.{}.json", selection.trim())),
        );
    }
    fs::write(
        attempt.join("selection-base"),
        format!("{selection_base}\n"),
    )?;
    fs::write(
        attempt.join("native-inputs.json"),
        serde_json::to_vec_pretty(
            &inputs
                .iter()
                .map(|input| {
                    serde_json::json!({
                        "worker": input.claim.name,
                        "source": input.source,
                        "scopes": input.claim.scopes,
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    let before = git(candidate, &["rev-parse", "HEAD"])?;
    let evidence = verify(candidate, attempt)?;
    let head = git(candidate, &["rev-parse", "HEAD"])?;
    if head != before
        || !git(
            candidate,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        return Err("verification changed the candidate tree; refusing promotion".into());
    }
    require_ship_evidence(&evidence)?;
    Ok((head, evidence))
}

/// Whether the re-gate after merging newer origin/main commits must run, and
/// a description of what it covers. The gates already passed on `tested`. A
/// package whose own files and dependencies the newly merged commits leave
/// unchanged builds and tests the same on the new head, so the re-gate's
/// selection is the change from `tested` to the candidate's new `HEAD`
/// (`affected.rs`; the root package's tree-wide tests are in every such
/// selection). When the merged tree equals the tested tree, or the change
/// selects no package, the gates already run cover the new head as is.
pub(in crate::local_workflow) fn regate_needed(candidate: &Path, tested: &str) -> (bool, String) {
    if env::var("SUSI_LOCAL_GATE").is_ok_and(|mode| mode == "full") {
        return (true, "SUSI_LOCAL_GATE=full".into());
    }
    let selection = affected::changed(candidate, tested, "HEAD").and_then(|changed| {
        let workspace = affected::load(candidate)?;
        Ok((changed.len(), affected::select(&workspace, &changed)))
    });
    match selection {
        Ok((0, _)) => (false, "the merged tree equals the tested tree".into()),
        Ok((changed, affected::Selection::Packages(packages))) if packages.is_empty() => {
            (false, format!("{changed} changed paths select no package"))
        }
        Ok((changed, affected::Selection::Packages(packages))) => (
            true,
            format!(
                "{changed} changed paths select {} packages: {}",
                packages.len(),
                packages.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        ),
        Ok((changed, affected::Selection::Workspace(reason))) => (
            true,
            format!("{changed} changed paths select the whole workspace ({reason})"),
        ),
        Err(error) => (true, format!("selection unavailable ({error})")),
    }
}

/// How many times a ship re-merges onto a moved origin/main and re-gates
/// before it gives up (nothing published) and asks to be run again.
pub(in crate::local_workflow) const SHIP_ROUNDS: usize = 5;

/// Verify and publish `claimants` to origin/main.
///
/// The gate runs in a private slot (`acquire_slot`) without any host or
/// repository queue, so concurrent ships gate side by side. Only publication
/// is serialized, under the repository's `publish` lock: re-fetch origin/main,
/// re-check the sources and reservations, record the receipts and move
/// `refs/susi/integration/current` in one ref transaction, push (a
/// fast-forward, never forced) and post statuses. If origin/main moved while
/// the gate ran, the lock is released, the sources are merged onto the new
/// origin/main and only what the newly merged commits affect is re-gated
/// (`regate_needed`); then publication is tried again.
pub(in crate::local_workflow) struct ShipRequest<'a> {
    pub(in crate::local_workflow) claimants: &'a [&'a str],
    pub(in crate::local_workflow) enforce_board: bool,
}

#[cfg(test)]
pub(in crate::local_workflow) fn ship<F>(
    root: &Path,
    claimants: &[&str],
    remote: &mut dyn Remote,
    mut verify: F,
) -> Result<ShipReport>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    ship_with_board_policy(root, claimants, remote, false, &mut verify)
}

pub(in crate::local_workflow) fn ship_with_board_policy<F>(
    root: &Path,
    claimants: &[&str],
    remote: &mut dyn Remote,
    enforce_board: bool,
    mut verify: F,
) -> Result<ShipReport>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let slot = timed("take verification slot", || acquire_slot(root))
        .map_err(|error| format!("slot: {error}"))?;
    eprintln!(
        "ship: verifying in {} (no queue; other ships gate at the same time)",
        slot.candidate.display()
    );
    // Native gate records of the slot's previous ship are not this ship's.
    if let Ok(entries) = fs::read_dir(&slot.attempt) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("native-gates") && name.ends_with(".json") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let req = ShipRequest {
        claimants,
        enforce_board,
    };
    let result = ship_in_slot(root, &slot, &req, remote, &mut verify);
    timed("stash warm target", || {
        remove_candidate(root, &slot.candidate)
    });
    result
}

/// A workspace proof binds commit identity, not just the tree. Retain old
/// evidence for audit, but never expose its machine-readable key after remerge.
pub(in crate::local_workflow) fn retire_workspace_proofs(
    evidence: &mut [String],
    tested: &str,
    merged: &str,
) {
    if tested == merged {
        return;
    }
    for line in evidence {
        if line.starts_with(workspace_test_proof::PREFIX)
            || line.starts_with("workspace-test-attestation-v1=")
        {
            *line = format!("historical workspace proof head={tested}: {line}");
        }
    }
}
pub(in crate::local_workflow) fn ship_in_slot<F>(
    root: &Path,
    slot: &Slot,
    req: &ShipRequest<'_>,
    remote: &mut dyn Remote,
    verify: &mut F,
) -> Result<ShipReport>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let claimants = req.claimants;
    let workflow = state(root).map_err(|error| format!("state: {error}"))?;
    let workflow = workflow.as_path();
    // Fetches update `refs/remotes/origin/main`; two at once can collide on
    // its ref lock. The first fetch takes a lock of its own (seconds), not
    // `publish`: a launcher built before slots holds `publish` for its whole
    // gate, and a new ship must not wait for that before its own gate. The
    // later fetches run under `publish`.
    let mut base = {
        let _fetch = timed("wait for fetch lock", || {
            Lock::acquire(&workflow.join("fetch"))
        })
        .map_err(|error| format!("lock: {error}"))?;
        timed("fetch origin/main", || remote.fetch_main(root))
            .map_err(|error| format!("fetch: {error}"))?
    };
    let inputs = timed("load sources", || {
        load_sources(root, workflow, claimants, &base)
    })
    .map_err(|error| format!("load: {error}"))?;
    if req.enforce_board {
        if let Some(first) = claimants.first() {
            cli_board_check(root, first)?;
        }
    }
    timed("merge candidate", || {
        merge_onto(root, &slot.candidate, &base, &inputs)
    })
    .map_err(|error| format!("merge: {error}"))?;
    let (mut head, mut evidence) =
        verify_merged(slot, &base, &inputs, verify).map_err(|error| format!("prove: {error}"))?;
    for round in 1..=SHIP_ROUNDS {
        let publish = timed("wait for publish lock", || {
            Lock::acquire(&workflow.join("publish"))
        })
        .map_err(|error| format!("lock: {error}"))?;
        timed("wait for legacy integration", || {
            wait_for_legacy_integration(workflow);
        });
        let current = {
            let _fetch = Lock::acquire(&workflow.join("fetch"))?;
            timed("fetch origin/main", || remote.fetch_main(root))?
        };
        if current == base {
            {
                let _claims = Lock::acquire(workflow)?;
                require_workflow(root, workflow)?;
                for input in &inputs {
                    if fs::read(&input.path)? != input.bytes
                        || check_ship_source(root, workflow, &input.claim, &base)? != input.source
                    {
                        return Err(
                            "source or reservation changed during verification; retry against its new head"
                                .into(),
                        );
                    }
                }
                require_workflow(root, workflow)?;
                timed("record acceptance", || {
                    record_acceptance(
                        root,
                        &slot.attempt,
                        &inputs,
                        PublicationHead {
                            head: &head,
                            observed_base: &base,
                        },
                        &evidence,
                    )
                })?;
            }
            let report = timed("publish", || publish_head(root, &head, &evidence, remote));
            drop(publish);
            return report;
        }
        drop(publish);
        if !is_ancestor(root, &base, &current)? {
            return Err(format!(
                "origin/main moved from {base} to {current}, which does not contain it; nothing published"
            )
            .into());
        }
        if round == SHIP_ROUNDS {
            break;
        }
        let tested = head.clone();
        eprintln!(
            "ship: origin/main moved from {base} to {current} during the gate; merging onto it and re-gating what the new commits affect"
        );
        timed("re-merge candidate", || {
            merge_onto(root, &slot.candidate, &current, &inputs)
        })
        .map_err(|error| format!("re-merge: {error}"))?;
        let merged = git(&slot.candidate, &["rev-parse", "HEAD"])?;
        retire_workspace_proofs(&mut evidence, &tested, &merged);
        let (needed, covers) = timed("re-gate selection", || {
            regate_needed(&slot.candidate, &tested)
        });
        // One evidence line, never shaped like a gate status or a receipt
        // key: `covers` can carry cargo's error text.
        let covers: String = covers
            .replace(" => ", " -> ")
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        evidence.push(format!(
            "regate base={current} tested={tested} head={merged} {}: {covers}",
            if needed { "ran" } else { "skipped" }
        ));
        if needed {
            let (regated, more) = verify_merged(slot, &tested, &inputs, verify)
                .map_err(|error| format!("re-gate: {error}"))?;
            head = regated;
            evidence.extend(more);
        } else {
            head = merged;
        }
        base = current;
    }
    Err(format!(
        "origin/main moved during each of {SHIP_ROUNDS} gate rounds; nothing published; run ship again"
    )
    .into())
}

pub(in crate::local_workflow) fn ship_retry(
    root: &Path,
    remote: &mut dyn Remote,
) -> Result<ShipReport> {
    let workflow = state(root)?;
    let _publish = Lock::acquire(&workflow.join("publish"))?;
    let Some(head) = integration_head(root)? else {
        return Err("missing receipt".into());
    };
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let receipt = matching_receipt(root, &head, &tree)?.ok_or("missing receipt")?;
    let evidence: Vec<String> = receipt
        .lines()
        .skip(7)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    require_ship_evidence(&evidence)?;
    let base = remote.fetch_main(root)?;
    if !is_ancestor(root, &base, &head)? {
        return Err("integration head is not based on origin/main".into());
    }
    let again = remote.fetch_main(root)?;
    if again != base {
        return Err("origin/main moved twice".into());
    }
    publish_head(root, &head, &evidence, remote)
}

pub(in crate::local_workflow) fn publish_head(
    root: &Path,
    head: &str,
    evidence: &[String],
    remote: &mut dyn Remote,
) -> Result<ShipReport> {
    // What reaches origin/main is the receipt commit: the verified head plus
    // `.agents/receipts/<head>.json`. No receipt, no publication.
    let published = gate_receipt::receipt_commit(root, head)
        .map_err(|error| format!("gate receipt for {head}: {error}; nothing published"))?
        .ok_or_else(|| {
            format!(
                "no gate receipt commit recorded at {} for verified head {head}; this head was accepted without a receipt (older ship tool), run ship again; nothing published",
                gate_receipt::ref_for(head)
            )
        })?;
    // Every gate passed on the head being published (checked again here, before
    // the push, where the status posts used to read the same evidence after it).
    require_ship_evidence(evidence)?;
    remote.push_ref(root, &published, "refs/heads/main")?;
    Ok(ShipReport {
        head: published,
        merge: head.to_owned(),
    })
}

pub(in crate::local_workflow) fn print_ship(report: &ShipReport) {
    println!(
        "Shipped {} directly to origin/main: verified merge {}, gate receipt {} (verify with `cargo xtask verify-receipt {}`).",
        report.head,
        report.merge,
        gate_receipt::path_for(&report.merge),
        report.merge
    );
}
