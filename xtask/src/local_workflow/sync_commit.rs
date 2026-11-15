use super::*;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Serialize only integration, not agent edits/commits. Publish the tested commit
/// and its receipt together in one Git ref transaction. No primary checkout moves.
pub(in crate::local_workflow) fn integrate_with<F>(
    root: &Path,
    claimants: &[&str],
    verify: F,
) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    integrate_many_with(root, claimants, verify)
}

pub(in crate::local_workflow) fn integrate_many_with<F>(
    root: &Path,
    claimants: &[&str],
    verify: F,
) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    native_provenance::require_unmodified_history(root)?;
    let first = *claimants
        .first()
        .ok_or("at least one claimant is required")?;
    let mut unique = std::collections::BTreeSet::new();
    for claimant in claimants {
        name(claimant)?;
        if !unique.insert(*claimant) {
            return Err("duplicate integration claimant".into());
        }
    }
    let state = state(root)?;
    let _integration = Lock::acquire(&state.join("integration"))?;
    let inputs = {
        let _claims = Lock::acquire(&state)?;
        claimants
            .iter()
            .map(|claimant| {
                let path = state.join("claims").join(format!("{claimant}.claim"));
                let claim = read_local_claim(&path)?;
                let source = check_source(root, &claim)?;
                let bytes = fs::read(&path)?;
                Ok((claim, source, path, bytes))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let previous = integration_head(root)?;
    // New claims start from `claim_base`: a fetched main that descends from the
    // local head is the newest landing. Integrate onto that same commit.
    let base = match &previous {
        Some(_) => claim_base(root)?,
        None => inputs[0].0.base.clone(),
    };
    // Dependency chains must be rooted in accepted history. Two unaccepted
    // workers cannot authorize each other's common, unowned starting commit.
    let mut admitted = vec![false; inputs.len()];
    loop {
        let before = admitted.clone();
        for (index, (claim, _, _, _)) in inputs.iter().enumerate() {
            if is_ancestor(root, &claim.base, &base)? {
                admitted[index] = true;
            }
            for (other, (_, source, _, _)) in inputs.iter().enumerate() {
                if before[other] && is_ancestor(root, &claim.base, source)? {
                    admitted[index] = true;
                }
            }
        }
        if admitted.iter().all(|value| *value) {
            break;
        }
        if admitted == before {
            return Err(
                "integration history does not contain the claim base: publish the local integration, fetch, then integrate again".into(),
            );
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let attempt = state
        .join("attempts")
        .join(format!("{first}-{stamp}-{}", std::process::id()));
    fs::create_dir_all(&attempt)?;
    let candidate = candidate_path(root)?;

    clear_candidate(root, &candidate)?;
    git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "--detach",
            text(&candidate)?,
            &base,
        ],
    )?;
    fs::write(
        attempt.join("inputs.txt"),
        inputs
            .iter()
            .map(|(claim, source, _, _)| {
                format!(
                    "agent={}\nclaim-base={}\nsource={source}\nintegration-base={base}\n",
                    claim.name, claim.base
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    fs::write(attempt.join("selection-base"), format!("{base}\n"))?;
    fs::write(
        attempt.join("native-inputs.json"),
        serde_json::to_vec_pretty(
            &inputs
                .iter()
                .map(|(claim, source, _, _)| {
                    serde_json::json!({
                        "worker":claim.name,"source":source,"scopes":claim.scopes
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    for (_, source, _, _) in &inputs {
        let merged = git(
            &candidate,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "user.name=Susi Local Integration",
                "-c",
                "user.email=susi-local@localhost",
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-ff",
                "--no-edit",
                source,
            ],
        );
        if let Err(error) = merged {
            fs::write(attempt.join("status"), format!("merge failed: {error}\n"))?;
            return Err(format!(
                "merge failed; source and reservation retained; repair evidence at {}: {error}",
                candidate.display()
            )
            .into());
        }
    }
    let head = git(&candidate, &["rev-parse", "HEAD"])?;
    let tree = git(&candidate, &["rev-parse", "HEAD^{tree}"])?;
    let mut evidence = match verify(&candidate, &attempt) {
        Ok(evidence) => evidence,
        Err(error) => {
            fs::write(
                attempt.join("status"),
                format!("verification failed: {error}\n"),
            )?;
            return Err(error);
        }
    };
    if evidence.is_empty() {
        return Err("verification returned no evidence".into());
    }
    if git(&candidate, &["rev-parse", "HEAD"])? != head
        || !git(
            &candidate,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        return Err("verification changed the candidate tree; refusing promotion".into());
    }
    // Ships no longer hold `integration` while they gate; they move the
    // accepted ref under `publish`. Take it (before the claims lock, the order
    // ship uses) so the ref transaction below cannot race a ship's.
    let _publish = Lock::acquire(&state.join("publish"))?;
    if integration_head(root)? != previous {
        return Err(
            "the accepted integration moved during verification (a ship published); run integrate again"
                .into(),
        );
    }
    let _claims = Lock::acquire(&state)?;
    for (claim, source, path, bytes) in &inputs {
        if fs::read(path)? != *bytes || check_source(root, claim)? != *source {
            return Err(
                "source or reservation changed during verification; retry against its new head"
                    .into(),
            );
        }
    }
    // Sign only after the full gate, sysroot recheck, clean tree and claim checks.
    if let Some(key) = evidence
        .iter()
        .find_map(|line| line.strip_prefix(workspace_test_proof::PREFIX))
        .map(str::to_owned)
    {
        if let Ok(attestation) = sign_workspace_pass(&candidate, &key) {
            evidence.push(format!(
                "workspace-test-attestation-v1={}",
                serde_json::to_string(&attestation)?
            ));
        }
    }
    let zero = "0".repeat(head.len());
    let mut transaction = format!(
        "start\nupdate {INTEGRATION_REF} {head} {}\n",
        previous.as_deref().unwrap_or(&zero)
    );
    let mut receipts = Vec::new();
    let native_gates = if attempt.join("native-gates.json").is_file() {
        Some(serde_json::from_slice::<serde_json::Value>(&fs::read(
            attempt.join("native-gates.json"),
        )?)?)
    } else {
        None
    };
    for (claim, source, _, bytes) in &inputs {
        let claimant = &claim.name;
        let receipt = format!(
            "susi-local-integration-v1\nagent={claimant}\nsource={source}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n",
            claim.base,
            previous.as_ref().map_or("none", |_| base.as_str()),
            evidence.join("\n")
        );
        let oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
        let receipt_ref = format!("refs/susi/integration/receipts/{claimant}/{source}");
        transaction.push_str(&format!("create {receipt_ref} {oid}\n"));
        receipts.push(receipt_ref);
        if let Some(gates) = &native_gates {
            let native = native_provenance::receipt(
                root,
                native_provenance::IntegrationSource {
                    worker: claimant,
                    claim_bytes: bytes,
                    source,
                    scope_base: &scope_base(root, claim, source)?,
                    previous: previous.as_deref(),
                    accepted: &head,
                },
                gates,
            )?;
            let native_oid = native_provenance::store_blob(root, &serde_json::to_vec(&native)?)?;
            let native_ref = format!("refs/susi/provenance/receipts/{claimant}/{source}");
            transaction.push_str(&format!("create {native_ref} {native_oid}\n"));
            receipts.push(native_ref);
        }
    }
    transaction.push_str("prepare\ncommit\n");
    git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())?;
    fs::write(
        attempt.join("status"),
        format!(
            "accepted {head}\nreceipt {}\nreceipts {}\n",
            receipts.join("\nreceipt "),
            receipts.join(" ")
        ),
    )?;
    for (_, _, path, _) in &inputs {
        fs::remove_file(path)?;
    }
    // The accepted commit and receipt are durable; retain failure candidates,
    // but reclaim successful scratch builds without touching agent worktrees.
    stash_candidate_target(root, &candidate);
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "-c",
            "core.longpaths=true",
            "worktree",
            "remove",
            "--force",
            text(&candidate)?,
        ],
    ) {
        eprintln!(
            "accepted integration candidate retained at {}: {error}",
            candidate.display()
        );
    }
    println!(
        "Integrated {}: {head}; receipts {}. Primary checkout unchanged.",
        claimants.join(", "),
        receipts.join(", ")
    );
    Ok(head)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::local_workflow) enum RemoteFetchState {
    NoRemoteConfigured,
    OfflineFallback { cached_main: Option<String> },
    Fetched { origin_main: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::local_workflow) enum AgentSyncMode {
    NoRemoteConfigured,
    OfflineFallback,
    UpToDate,
    FastForwarded,
    MergedThreeWay,
}

impl AgentSyncMode {
    pub(in crate::local_workflow) fn as_str(self) -> &'static str {
        match self {
            Self::NoRemoteConfigured => "no_remote_configured",
            Self::OfflineFallback => "offline_fallback",
            Self::UpToDate => "up_to_date",
            Self::FastForwarded => "fast_forwarded",
            Self::MergedThreeWay => "merged_three_way",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::local_workflow) struct AgentSyncReport {
    pub(in crate::local_workflow) claimant: String,
    pub(in crate::local_workflow) mode: AgentSyncMode,
    pub(in crate::local_workflow) previous_head: String,
    pub(in crate::local_workflow) head: String,
    pub(in crate::local_workflow) upstream: Option<String>,
    pub(in crate::local_workflow) claim_base: String,
}

pub(in crate::local_workflow) fn has_origin_remote(root: &Path) -> bool {
    Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(root)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Offline-safe pure-Git fetch of `origin`'s `main` into `refs/remotes/origin/main`.
/// Never contacts any host REST API or `gh` CLI; falls back cleanly to local cached
/// refs when `origin` is absent or temporarily unreachable.
pub(in crate::local_workflow) fn fetch_origin_optional(
    root: &Path,
    workflow_state: &Path,
) -> Result<RemoteFetchState> {
    if !has_origin_remote(root) {
        return Ok(RemoteFetchState::NoRemoteConfigured);
    }
    let _fetch_lock = Lock::acquire(&workflow_state.join("fetch"))?;
    let status = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=",
            "fetch",
            "--quiet",
            "--no-tags",
            "--recurse-submodules=no",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(st) if st.success() => match fetched_main(root)? {
            Some(oid) if valid_oid(&oid) => Ok(RemoteFetchState::Fetched { origin_main: oid }),
            cached_main => Ok(RemoteFetchState::OfflineFallback { cached_main }),
        },
        _ => Ok(RemoteFetchState::OfflineFallback {
            cached_main: fetched_main(root)?,
        }),
    }
}

pub(in crate::local_workflow) fn can_advance_claim_base(root: &Path, target: &str) -> Result<bool> {
    match integration_head(root)? {
        Some(integrated) => Ok(integrated == target || is_ancestor(root, &integrated, target)?),
        None => Ok(true),
    }
}

pub(in crate::local_workflow) fn reconcile_primary_with_origin(
    root: &Path,
    workflow_state: &Path,
) -> Result<RemoteFetchState> {
    let fetch_state = fetch_origin_optional(root, workflow_state)?;
    if let RemoteFetchState::Fetched { ref origin_main } = fetch_state {
        if can_advance_claim_base(root, origin_main)? {
            let local_main_ok = match Command::new("git")
                .arg("--literal-pathspecs")
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .current_dir(root)
                .args(["rev-parse", "--verify", "--quiet", "refs/heads/main"])
                .output()
            {
                Ok(out) if out.status.success() => {
                    let cur = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                    cur.is_empty() || cur == *origin_main || is_ancestor(root, &cur, origin_main)?
                }
                _ => true,
            };
            if local_main_ok {
                sync_local_main(root, origin_main);
            }
        }
    }
    Ok(fetch_state)
}

pub(in crate::local_workflow) fn write_local_claim_atomic(
    path: &Path,
    claim: &Claim,
) -> Result<()> {
    let data = format!(
        "susi-local-claim-v2\n{}\n{}\n{}\n{}\n",
        claim.name,
        text(&claim.worktree)?,
        claim.base,
        claim.scopes.join("\n")
    );
    let temporary = path.with_extension("claim-next");
    let _ = fs::remove_file(&temporary);
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(data.as_bytes())?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, path)?;
    Ok(())
}

pub(in crate::local_workflow) fn reconcile_claimed_worktree_locked(
    root: &Path,
    workflow_state: &Path,
    claimant: &str,
    fetch_state: &RemoteFetchState,
) -> Result<AgentSyncReport> {
    let claim_path = workflow_state
        .join("claims")
        .join(format!("{claimant}.claim"));
    let mut claim = read_local_claim(&claim_path)?;
    if self::state(&claim.worktree)? != workflow_state
        || git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("claim repository or branch does not match".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(
            "cannot reconcile remote main into a dirty agent worktree; commit or clean first"
                .into(),
        );
    }
    let markers = [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "rebase-merge",
        "rebase-apply",
    ];
    for marker_path in git_paths(&claim.worktree, &markers)? {
        if marker_path.exists() {
            return Err("finish the existing Git operation before syncing".into());
        }
    }

    let head = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    let upstream = claim_base(root)?;
    if upstream == head || is_ancestor(root, &upstream, &head)? {
        let mode = match fetch_state {
            RemoteFetchState::NoRemoteConfigured => AgentSyncMode::NoRemoteConfigured,
            RemoteFetchState::OfflineFallback { .. } => AgentSyncMode::OfflineFallback,
            RemoteFetchState::Fetched { .. } => AgentSyncMode::UpToDate,
        };
        return Ok(AgentSyncReport {
            claimant: claimant.to_owned(),
            mode,
            previous_head: head.clone(),
            head,
            upstream: Some(upstream),
            claim_base: claim.base,
        });
    }

    if !is_ancestor(root, &claim.base, &upstream)? {
        return Err(format!(
            "upstream {upstream} does not descend from reserved claim base {}; refusing non-fast-forward reconciliation",
            claim.base
        )
        .into());
    }

    if is_ancestor(root, &head, &upstream)? {
        git(
            &claim.worktree,
            &[
                "-c",
                "core.hooksPath=",
                "merge",
                "--ff-only",
                "--quiet",
                &upstream,
            ],
        )?;
        if can_advance_claim_base(root, &upstream)? {
            claim.base = upstream.clone();
            write_local_claim_atomic(&claim_path, &claim)?;
        }
        return Ok(AgentSyncReport {
            claimant: claimant.to_owned(),
            mode: AgentSyncMode::FastForwarded,
            previous_head: head,
            head: upstream.clone(),
            upstream: Some(upstream),
            claim_base: claim.base,
        });
    }

    // Diverged: validate agent's committed scope delta first, then perform clean
    // non-conflicting 3-way merge (`git merge-tree --write-tree`) with upstream.
    check_ship_source(root, workflow_state, &claim, &upstream)?;
    let merge_out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(&claim.worktree)
        .args(["merge-tree", "--write-tree", &head, &upstream])
        .output()
        .map_err(|error| format!("git merge-tree in {}: {error}", claim.worktree.display()))?;
    if !merge_out.status.success() {
        return Err(format!(
            "upstream main ({upstream}) conflicts with local-agent/{claimant}; resolve with `git merge --no-ff --no-commit {upstream}` and `local-workflow finish-merge {claimant} {upstream} <message>`"
        )
        .into());
    }
    let merged_tree = String::from_utf8_lossy(&merge_out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if !valid_oid(&merged_tree) {
        return Err("git merge-tree returned an invalid tree oid".into());
    }
    let diff_out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(&claim.worktree)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &upstream,
            &merged_tree,
        ])
        .output()?;
    if !diff_out.status.success() {
        return Err("cannot inspect reconciled merge scope".into());
    }
    for raw_path in diff_out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let normalized = scope(std::str::from_utf8(raw_path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &normalized)) {
            return Err(
                format!("reconciled merge file is outside reserved scope: {normalized}").into(),
            );
        }
    }
    let hooks = workflow_state.join("empty-hooks");
    fs::create_dir_all(&hooks)?;
    if fs::symlink_metadata(&hooks)?.file_type().is_symlink()
        || fs::read_dir(&hooks)?.next().is_some()
    {
        return Err("local empty-hooks directory must be an empty real directory".into());
    }
    let merge_msg =
        format!("sync local-agent/{claimant} with upstream main\n\nTask: LOCAL-{claimant}\n");
    let merge_commit = git_input(
        &claim.worktree,
        &[
            "--no-replace-objects",
            "commit-tree",
            &merged_tree,
            "-p",
            &head,
            "-p",
            &upstream,
            "-F",
            "-",
        ],
        merge_msg.as_bytes(),
    )?;
    git(
        &claim.worktree,
        &[
            "-c",
            &format!("core.hooksPath={}", text(&hooks)?),
            "merge",
            "--ff-only",
            "--quiet",
            &merge_commit,
        ],
    )?;
    if can_advance_claim_base(root, &upstream)? {
        claim.base = upstream.clone();
        write_local_claim_atomic(&claim_path, &claim)?;
    }
    Ok(AgentSyncReport {
        claimant: claimant.to_owned(),
        mode: AgentSyncMode::MergedThreeWay,
        previous_head: head,
        head: merge_commit,
        upstream: Some(upstream),
        claim_base: claim.base,
    })
}

pub(in crate::local_workflow) fn sync_claim(
    root: &Path,
    claimant: &str,
) -> Result<AgentSyncReport> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let workflow_state = state(root)?;
    let fetch_state = reconcile_primary_with_origin(root, &workflow_state)?;
    let _lock = Lock::acquire(&workflow_state)?;
    reconcile_claimed_worktree_locked(root, &workflow_state, claimant, &fetch_state)
}

pub(in crate::local_workflow) fn workflow_distill(
    root: &Path,
    distill_args: &[String],
) -> Result<String> {
    native_provenance::require_unmodified_history(root)?;
    let workflow_state = state(root)?;
    let _ = reconcile_primary_with_origin(root, &workflow_state)?;
    if distill_args.first().map(String::as_str) == Some("lint") {
        let cargo_bin = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo_bin)
            .current_dir(root)
            .args([
                "run",
                "--offline",
                "--locked",
                "-p",
                "xtask",
                "--bin",
                "distill",
                "--",
            ])
            .args(distill_args)
            .output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .trim()
                .to_owned()
                .into());
        }
        Ok(String::from_utf8(out.stdout)?.trim_end().to_owned())
    } else {
        let mut forwarded: Vec<String> = distill_args.to_vec();
        if matches!(
            forwarded.first().map(String::as_str),
            Some("plan" | "compose" | "orchestrate")
        ) && !forwarded.iter().any(|arg| arg == "--pull")
        {
            forwarded.push("--pull".to_owned());
        }
        distill_compose::run(root, &forwarded).map_err(Into::into)
    }
}

pub(in crate::local_workflow) fn start(
    root: &Path,
    claimant: &str,
    raw_scopes: &[String],
) -> Result<PathBuf> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let mut scopes = raw_scopes
        .iter()
        .map(|s| scope(s))
        .collect::<Result<Vec<_>>>()?;
    if scopes.is_empty() {
        return Err("at least one --scope is required".into());
    }
    scopes.sort();
    scopes.dedup();
    let state = state(root)?;
    let _ = reconcile_primary_with_origin(root, &state)?;
    let _lock = Lock::acquire(&state)?;
    for active in claims(&state)? {
        if active.name == claimant {
            return Err(format!("agent {claimant} already has a claim").into());
        }
        for requested in &scopes {
            if active.scopes.iter().any(|held| overlaps(requested, held)) {
                return Err(format!("scope {requested} overlaps agent {}", active.name).into());
            }
        }
    }
    // Agent worktrees sit beside the primary checkout, whichever worktree
    // `start` runs from (agents never run it inside the primary).
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let primary = match common.file_name() {
        Some(name) if name == ".git" => common
            .parent()
            .ok_or("git common dir has no parent")?
            .to_path_buf(),
        _ => PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?),
    };
    let base = primary
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid repository name")?;
    let worktree = primary
        .parent()
        .ok_or("repository has no parent")?
        .join(format!("{base}-agent-{claimant}"));
    let branch = format!("local-agent/{claimant}");
    let base_oid = claim_base(root)?;
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let worktree_text = text(&worktree)?;
    if worktree_text.chars().any(char::is_control) {
        return Err("worktree path contains control characters".into());
    }
    let claim_text = format!(
        "susi-local-claim-v2\n{claimant}\n{worktree_text}\n{base_oid}\n{}\n",
        scopes.join("\n")
    );
    if let Ok(meta) = fs::symlink_metadata(&worktree) {
        if !reusable_worktree(root, &worktree, &meta, &branch, &base_oid)? {
            return Err("destination already exists; refusing to reuse it".into());
        }
        // Reuse the released checkout so its warm target/ survives.
        fs::write(&claim_path, &claim_text)?;
        if let Err(error) = git(
            &worktree,
            &[
                "-c",
                "core.hooksPath=",
                "checkout",
                "-B",
                &branch,
                &base_oid,
            ],
        ) {
            fs::remove_file(&claim_path)?;
            return Err(error);
        }
        fs::create_dir_all(worktree.join("target"))?;
        seed_worktree_target(&primary, &worktree, true);
        return Ok(worktree);
    }
    fs::write(&claim_path, &claim_text)?;
    // Suppress legacy shell/network hooks for this local creation only. No
    // persistent Git setting changes and no remote operation are performed.
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "-b",
            &branch,
            worktree_text,
            &base_oid,
        ],
    ) {
        // Retain the reservation if Git left a checkout behind, for inspection.
        if !worktree.exists() {
            fs::remove_file(claim_path)?;
        }
        return Err(error);
    }
    fs::create_dir(worktree.join("target"))?;
    seed_worktree_target(&primary, &worktree, true);
    Ok(worktree)
}

/// An existing destination is reused only when it is a real directory that
/// this repository registers as a worktree, has no uncommitted or untracked
/// work, and resetting `branch` to `base_oid` drops no commit: neither the
/// agent branch tip nor a detached HEAD may hold work outside `base_oid`.
pub(in crate::local_workflow) fn reusable_worktree(
    root: &Path,
    worktree: &Path,
    meta: &fs::Metadata,
    branch: &str,
    base_oid: &str,
) -> Result<bool> {
    if meta.file_type().is_symlink() || !meta.is_dir() || !registered_worktree(root, worktree)? {
        return Ok(false);
    }
    if !git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .trim()
    .is_empty()
    {
        return Ok(false);
    }
    let branch_ref = format!("refs/heads/{branch}");
    if let Ok(tip) = git(root, &["rev-parse", "--verify", "--quiet", &branch_ref]) {
        if !tip.is_empty() && !is_ancestor(root, &tip, base_oid)? {
            return Err(
                format!("{branch} holds commits outside {base_oid}; refusing to reset it").into(),
            );
        }
    }
    if git(worktree, &["symbolic-ref", "--quiet", "HEAD"]).is_err() {
        let head = git(worktree, &["rev-parse", "HEAD"])?;
        if !is_ancestor(root, &head, base_oid)? {
            return Err("reusable worktree has a detached HEAD with unshipped commits".into());
        }
    }
    Ok(true)
}

pub(in crate::local_workflow) fn registered_worktree(root: &Path, worktree: &Path) -> Result<bool> {
    let wanted = worktree.canonicalize()?;
    Ok(git(root, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| {
            Path::new(listed)
                .canonicalize()
                .is_ok_and(|listed| listed == wanted)
        }))
}

pub(in crate::local_workflow) fn seed_worktree_target(
    primary: &Path,
    worktree: &Path,
    background: bool,
) {
    let Some(parent) = primary.parent() else {
        return;
    };
    if !parent.join(".susi-target-seed/.susi-seed").is_file()
        || worktree.join("target/.susi-seeded-from").is_file()
    {
        return;
    }
    // `workflow seed-target` is Susi Commercial Apps' dev workflow
    // (`susi-commercial`) since the lean-core move; an installed core `susi`
    // older than the move still carries it. The first binary that answers
    // the verb seeds; with none, the worktree keeps a cold target.
    let installed = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|local| local.join("SusiLocal/production/entry/susi.exe"))
        .filter(|path| path.is_file());
    let has_seed = |bin: &Path| {
        Command::new(bin)
            .args(["workflow", "seed-target", "--help"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let Some(seed_bin) = [
        Some(PathBuf::from("susi-commercial")),
        installed,
        Some(PathBuf::from("susi")),
    ]
    .into_iter()
    .flatten()
    .find(|bin| has_seed(bin)) else {
        return;
    };
    let mut cmd = Command::new(seed_bin);
    cmd.arg("workflow").arg("seed-target");
    if background {
        cmd.arg("--background");
    }
    cmd.arg(worktree);
    let _ = cmd.status();
}

pub(in crate::local_workflow) enum ScopeChange {
    Extend,
    Shrink,
}

#[cfg(test)]
pub(in crate::local_workflow) fn extend(
    root: &Path,
    claimant: &str,
    raw_scopes: &[String],
) -> Result<()> {
    update_scopes(root, claimant, raw_scopes, ScopeChange::Extend)
}

pub(in crate::local_workflow) fn update_scopes(
    root: &Path,
    claimant: &str,
    raw_scopes: &[String],
    change: ScopeChange,
) -> Result<()> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let requested = raw_scopes
        .iter()
        .map(|value| scope(value))
        .collect::<Result<Vec<_>>>()?;
    if requested.is_empty() {
        return Err("at least one --scope is required".into());
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let path = state.join("claims").join(format!("{claimant}.claim"));
    let mut claim = read_local_claim(&path)?;
    let actual = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual.canonicalize()? != claim.worktree.canonicalize()?
        || git(root, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("scope changes must run inside the claimed agent branch and worktree".into());
    }
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("scope changes require a clean agent worktree".into());
    }
    match change {
        ScopeChange::Extend => {
            for active in claims(&state)? {
                if active.name != claimant
                    && requested
                        .iter()
                        .any(|new| active.scopes.iter().any(|held| overlaps(new, held)))
                {
                    return Err(format!("requested scope overlaps agent {}", active.name).into());
                }
            }
            claim.scopes.extend(requested);
        }
        ScopeChange::Shrink => {
            if requested.iter().any(|scope| !claim.scopes.contains(scope)) {
                return Err("shrink requires an exact held scope".into());
            }
            claim.scopes.retain(|scope| !requested.contains(scope));
            if claim.scopes.is_empty() {
                return Err("use release to relinquish the last scope".into());
            }
            if git(&claim.worktree, &["rev-parse", "HEAD"])? != claim.base {
                // Validate the proposed claim against the actual committed
                // delta before atomically replacing the reservation.
                check_source(root, &claim)?;
            }
        }
    }
    claim.scopes.sort();
    claim.scopes.dedup();
    let data = format!(
        "susi-local-claim-v2\n{}\n{}\n{}\n{}\n",
        claim.name,
        text(&claim.worktree)?,
        claim.base,
        claim.scopes.join("\n")
    );
    let temporary = path.with_extension("claim-next");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(data.as_bytes())?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, path)?;
    Ok(())
}

pub(in crate::local_workflow) fn release(root: &Path, claimant: &str) -> Result<()> {
    name(claimant)?;
    let state = state(root)?;
    let _ = reconcile_primary_with_origin(root, &state)?;
    let _lock = Lock::acquire(&state)?;
    let path = state.join("claims").join(format!("{claimant}.claim"));
    let claim = read_local_claim(&path)?;
    if self::state(&claim.worktree)? != state {
        return Err("claim points to another repository".into());
    }
    if git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}") {
        return Err("worktree branch changed; inspect before releasing".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("worktree is dirty; preserve and commit its work before releasing".into());
    }
    let head = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if head != claim.base && !landed(root, &head)? {
        return Err(
            "committed work is neither in local integration nor on the fetched origin/main; reservation retained"
                .into(),
        );
    }
    fs::remove_file(path)?;
    println!(
        "Released {claimant}; retained branch and worktree {}",
        claim.worktree.display()
    );
    Ok(())
}

/// Collect each index and working-tree delta separately: their combined HEAD
/// diff can be empty even when the index still contains a staged change.
#[derive(Debug, Default, Eq, PartialEq)]
pub(in crate::local_workflow) struct PathChanges {
    /// All staged, unstaged and untracked paths for authorization checks.
    pub(in crate::local_workflow) all: Vec<String>,
    /// Paths from the worktree and untracked sets that still need `git add`.
    /// Staged paths are validated but must not be resubmitted as pathspecs:
    /// a staged deletion no longer exists, and can make `git add -A` fail.
    pub(in crate::local_workflow) to_stage: Vec<String>,
}

pub(in crate::local_workflow) fn changed_paths(root: &Path, base: &str) -> Result<Vec<String>> {
    Ok(changed_path_changes(root, base)?.all)
}

pub(in crate::local_workflow) fn changed_path_changes(
    root: &Path,
    base: &str,
) -> Result<PathChanges> {
    let mut changes = PathChanges::default();
    for (index, args) in [
        vec![
            "diff",
            "--cached",
            "--no-renames",
            "--name-only",
            "-z",
            base,
            "--",
        ],
        vec!["diff", "--no-renames", "--name-only", "-z"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ]
    .into_iter()
    .enumerate()
    {
        let out = Command::new("git")
            .arg("--literal-pathspecs")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .current_dir(root)
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(format!(
                "cannot inspect changes: {}",
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        for path in out
            .stdout
            .split(|byte| *byte == 0)
            .filter(|p| !p.is_empty())
        {
            let path = String::from_utf8(path.to_vec())?;
            changes.all.push(path.clone());
            if index != 0 {
                changes.to_stage.push(path);
            }
        }
    }
    changes.all.sort();
    changes.all.dedup();
    changes.to_stage.sort();
    changes.to_stage.dedup();
    Ok(changes)
}

pub(in crate::local_workflow) fn owns_path(held: &str, changed: &str) -> bool {
    held == changed
        || changed
            .strip_prefix(held)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub(in crate::local_workflow) fn claimed_changes(claim: &Claim) -> Result<Vec<String>> {
    claimed_changes_against(claim, "HEAD")
}

pub(in crate::local_workflow) fn claimed_changes_against(
    claim: &Claim,
    base: &str,
) -> Result<Vec<String>> {
    Ok(claimed_path_changes_against(claim, base)?.all)
}

pub(in crate::local_workflow) fn claimed_path_changes(claim: &Claim) -> Result<PathChanges> {
    claimed_path_changes_against(claim, "HEAD")
}

pub(in crate::local_workflow) fn claimed_path_changes_against(
    claim: &Claim,
    base: &str,
) -> Result<PathChanges> {
    let changes = changed_path_changes(&claim.worktree, base)?;
    for path in &changes.all {
        let normalized = scope(path)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &normalized)) {
            return Err(format!(
                "changed file is outside {}'s reserved scopes: {path}",
                claim.name
            )
            .into());
        }
    }
    Ok(changes)
}

/// Scope-guarded local commit, not a test/release gate. It never changes the
/// repository's shared hook configuration or invokes its network-bound hooks.
pub(in crate::local_workflow) fn find_susi_board_bin(root: &Path) -> Option<PathBuf> {
    let exe = format!("susi{}", env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    for key in ["SUSI_WORKFLOW_BIN", "SUSI_BIN"] {
        if let Ok(val) = env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }
    }
    candidates.push(root.join("target/release").join(&exe));
    candidates.push(root.join("target/debug").join(&exe));
    if let Ok(path_env) = env::var("PATH") {
        for dir in env::split_paths(&path_env) {
            candidates.push(dir.join(&exe));
        }
    }
    candidates.into_iter().find(|bin| {
        bin.is_file()
            && Command::new(bin)
                .args(["board", "check", "--help"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    })
}

pub(in crate::local_workflow) fn cli_board_check(root: &Path, agent: &str) -> Result<()> {
    let Some(bin) = find_susi_board_bin(root) else {
        return Err(
            "mandatory Susi Native Board check failed: build `susi` (`cargo build`) or set SUSI_BIN"
                .into(),
        );
    };
    let out = Command::new(&bin)
        .current_dir(root)
        .args(["board", "check", "--agent", agent])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("Susi Native Board refused workflow action: {detail}").into());
    }
    Ok(())
}

pub(in crate::local_workflow) fn cli_board_post(
    root: &Path,
    agent: &str,
    kind: &str,
    title: &str,
    body: &str,
) -> Result<()> {
    let Some(bin) = find_susi_board_bin(root) else {
        return Err(
            "mandatory Susi Native Board post failed: build `susi` (`cargo build`) or set SUSI_BIN"
                .into(),
        );
    };
    let out = Command::new(&bin)
        .current_dir(root)
        .args([
            "board", "post", "--kind", kind, "--agent", agent, "--title", title, "--body", body,
        ])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("Susi Native Board post failed closed: {detail}").into());
    }
    Ok(())
}

pub(in crate::local_workflow) fn commit(
    root: &Path,
    claimant: &str,
    message: &str,
    signer: Option<&str>,
) -> Result<()> {
    scoped_commit(root, claimant, message, signer, None)?;
    if has_origin_remote(root) {
        let _ = sync_claim(root, claimant)?;
    }
    Ok(())
}

pub(in crate::local_workflow) fn scoped_commit(
    root: &Path,
    claimant: &str,
    message: &str,
    signer: Option<&str>,
    merge_parent: Option<&str>,
) -> Result<()> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    if message.trim().is_empty() || message.contains('\0') {
        return Err("commit message must be nonempty and contain no NUL".into());
    }
    if let Some(key) = signer {
        if !matches!(key.len(), 40 | 64) || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("--sign requires a complete OpenPGP fingerprint".into());
        }
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let claim = read_local_claim(&claim_path)?;
    let actual_root = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual_root.canonicalize()? != claim.worktree.canonicalize()? {
        return Err("commit must run inside the claimed agent's worktree".into());
    }
    if self::state(&claim.worktree)? != state
        || git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("claim repository or branch does not match".into());
    }
    if signer.is_some() && merge_parent.is_some() {
        return Err("finish-merge does not support signing".into());
    }
    let signing = match signer {
        Some(key) => Some((key, git(&claim.worktree, &["rev-parse", "HEAD"])?)),
        None => None,
    };
    // Resolve all worktree-aware paths in one Git process, without caching.
    // Every marker remains checked here and rechecked before ref publication.
    let markers = [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "rebase-merge",
        "rebase-apply",
    ];
    let resolved = git_paths(&claim.worktree, &markers)?;
    let mut forbidden = Vec::new();
    for (marker, path) in markers.iter().zip(resolved) {
        if *marker == "MERGE_HEAD" && merge_parent.is_some() {
            continue;
        }
        if path.exists() {
            return Err("finish the existing Git operation before committing".into());
        }
        forbidden.push(path);
    }
    let changes = claimed_path_changes(&claim)?;
    if changes.all.is_empty() && merge_parent.is_none() {
        return Err("nothing to commit".into());
    }
    let hooks = state.join("empty-hooks");
    fs::create_dir_all(&hooks)?;
    if fs::symlink_metadata(&hooks)?.file_type().is_symlink()
        || fs::read_dir(&hooks)?.next().is_some()
    {
        return Err("local empty-hooks directory must be an empty real directory".into());
    }
    if let Some(expected) = merge_parent {
        return finish_scoped_merge(&claim, expected, message, &hooks, &forbidden);
    }
    // NUL-delimited literal pathspecs preserve spaces and metacharacters and
    // avoid the Windows command-line length limit for large tasks.
    if !changes.to_stage.is_empty() {
        let mut add = Command::new("git")
            .arg("--literal-pathspecs")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .current_dir(&claim.worktree)
            .args([
                "--literal-pathspecs",
                "add",
                "-A",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut input) = add.stdin.take() {
            for path in &changes.to_stage {
                input.write_all(path.as_bytes())?;
                input.write_all(&[0])?;
            }
        } else {
            return Err("could not open git add input".into());
        }
        let out = add.wait_with_output()?;
        if !out.status.success() {
            return Err(format!("git add failed: {}", String::from_utf8_lossy(&out.stderr)).into());
        }
    }
    let hook_config = format!("core.hooksPath={}", text(&hooks)?);
    let trailer = format!("Task: LOCAL-{claimant}");
    let output = if let Some((key, parent)) = signing {
        // Sign and verify a detached commit object before publishing it. A
        // missing signer, wrong identity or concurrent branch move leaves the
        // previous head intact and the worker can retry the staged change.
        let tree = git(&claim.worktree, &["write-tree"])?;
        let claim_bytes = fs::read(&claim_path)?;
        // An optional hardware signer can wait for its operator. Unrelated
        // workers must still be able to reserve, commit and release scopes.
        drop(_lock);
        let signed = git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "commit-tree",
                &tree,
                "-p",
                &parent,
                &format!("-S{key}"),
                "-m",
                message,
                "-m",
                &trailer,
            ],
        )?;
        git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "verify-commit",
                &signed,
            ],
        )?;
        let identities = git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "show",
                "-s",
                "--format=%GF%n%GP",
                &signed,
            ],
        )?;
        if !identities
            .lines()
            .any(|fingerprint| fingerprint.eq_ignore_ascii_case(key))
        {
            return Err("signed commit did not verify under the requested fingerprint".into());
        }
        let _publish = Lock::acquire(&state)?;
        if fs::read(&claim_path)? != claim_bytes
            || git(&claim.worktree, &["branch", "--show-current"])?
                != format!("local-agent/{claimant}")
            || git(&claim.worktree, &["write-tree"])? != tree
        {
            return Err("worker reservation, branch or staged tree changed while signing".into());
        }
        claimed_changes(&claim)?;
        git(
            &claim.worktree,
            &[
                "-c",
                &hook_config,
                "update-ref",
                &format!("refs/heads/local-agent/{claimant}"),
                &signed,
                &parent,
            ],
        )?;
        format!("Verified OpenPGP commit {signed} with fingerprint {key}.")
    } else {
        git(
            &claim.worktree,
            &[
                "-c",
                &hook_config,
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                message,
                "-m",
                &trailer,
            ],
        )?
    };
    println!(
        "{output}\nLocal scope guard passed; this commit is not a test or release attestation."
    );
    Ok(())
}

/// Finish only a resolved, explicitly pinned merge in the reserved worktree.
/// ORIG_HEAD lets a retry finish cleanup if publication already succeeded.
pub(in crate::local_workflow) fn finish_scoped_merge(
    claim: &Claim,
    expected: &str,
    message: &str,
    hooks: &Path,
    forbidden: &[PathBuf],
) -> Result<()> {
    let root = &claim.worktree;
    let claim_path = state(root)?
        .join("claims")
        .join(format!("{}.claim", claim.name));
    if !matches!(expected.len(), 40 | 64) || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("finish-merge requires a full commit object ID".into());
    }
    let object = git(
        root,
        &["rev-parse", "--verify", &format!("{expected}^{{commit}}")],
    )?;
    if object != expected {
        return Err("expected parent must name the commit itself".into());
    }
    let original = git(root, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])?;
    if original == expected {
        return Err("merge parents must be distinct".into());
    }
    git(
        root,
        &["merge-base", "--is-ancestor", &claim.base, &original],
    )?;
    let merge_head = git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "MERGE_HEAD",
        ],
    )?;
    let tree = git(root, &["write-tree"])?;
    let branch = format!("local-agent/{}", claim.name);
    let validate = || -> Result<()> {
        if read_claim(&claim_path)? != *claim
            || forbidden.iter().any(|p| p.exists())
            || fs::read_to_string(&merge_head)?.lines().collect::<Vec<_>>() != [expected]
            || git(root, &["branch", "--show-current"])? != branch
            || git(root, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])? != original
            || git(root, &["write-tree"])? != tree
        {
            return Err("merge parent, branch or staged tree changed".into());
        }
        for args in [
            &["ls-files", "--unmerged", "-z"][..],
            &["diff", "--name-only", "-z", "--"][..],
            &["ls-files", "--others", "--exclude-standard", "-z"][..],
        ] {
            if !git(root, args)?.is_empty() {
                return Err(
                    "finish-merge requires a resolved index and fully staged changes".into(),
                );
            }
        }
        claimed_changes_against(claim, &original)?;
        Ok(())
    };
    validate()?;
    let message = format!("{}\n\nTask: LOCAL-{}", message.trim(), claim.name);
    let expected_shape = format!("{tree}\n{original} {expected}\n{message}");
    let head = git(root, &["rev-parse", "HEAD"])?;
    let commit = if head == original {
        git_input(
            root,
            &[
                "--no-replace-objects",
                "commit-tree",
                &tree,
                "-p",
                &original,
                "-p",
                expected,
                "-F",
                "-",
            ],
            message.as_bytes(),
        )?
    } else {
        head.clone()
    };
    if git(
        root,
        &[
            "--no-replace-objects",
            "show",
            "-s",
            "--format=%T%n%P%n%B",
            &commit,
        ],
    )? != expected_shape
    {
        return Err("refusing a merge with different parents, tree or task message".into());
    }
    let hooks = format!("core.hooksPath={}", text(hooks)?);
    validate()?;
    if head == original {
        git(
            root,
            &[
                "-c",
                &hooks,
                "update-ref",
                &format!("refs/heads/{branch}"),
                &commit,
                &original,
            ],
        )?;
    }
    validate()?;
    if git(root, &["rev-parse", "HEAD"])? != commit {
        return Err("claimed branch moved before merge cleanup".into());
    }
    git(root, &["-c", &hooks, "merge", "--quit"])?;
    println!("Finished scoped merge {commit}, parents {original} {expected}, tree {tree}.");
    Ok(())
}
