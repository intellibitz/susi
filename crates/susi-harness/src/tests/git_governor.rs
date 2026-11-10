use super::*;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test Agent")
        .env("GIT_AUTHOR_EMAIL", "agent@susi.local")
        .env("GIT_COMMITTER_NAME", "Test Agent")
        .env("GIT_COMMITTER_EMAIL", "agent@susi.local")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn setup_storage_and_work(root: &Path) -> (PathBuf, PathBuf, String) {
    let remote_bare = root.join("origin.git");
    let work = root.join("work");
    std::fs::create_dir_all(&remote_bare).unwrap();
    git(&remote_bare, &["init", "--bare", "--initial-branch=main"]);
    git(
        root,
        &[
            "clone",
            remote_bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(&work, &["config", "user.name", "Test Agent"]);
    git(&work, &["config", "user.email", "agent@susi.local"]);

    std::fs::create_dir_all(work.join(".agents/tasks")).unwrap();
    std::fs::write(work.join("README.md"), "base\n").unwrap();
    std::fs::write(
        work.join(".agents/tasks/T-ADOPT-1.json"),
        serde_json::to_string_pretty(&json!({
            "id": "T-ADOPT-1",
            "results": [
                {
                    "branch": "origin/retired-wave",
                    "verdict": "RETIRE",
                    "evidence": "superseded by gated landing"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-m", "chore: initial\n\nTask: T-BASE-1"]);
    git(&work, &["push", "origin", "HEAD:refs/heads/main"]);
    let base_oid = git(&work, &["rev-parse", "HEAD"]);
    (remote_bare, work, base_oid)
}

#[test]
fn sweep_pending_branches_merges_clean_siblings_isolates_conflicts_and_skips_protected_and_retired()
{
    let tmp = tempfile::tempdir().unwrap();
    let (remote_bare, work, base_oid) = setup_storage_and_work(tmp.path());

    // 1. Clean branch A
    git(&work, &["checkout", "-b", "local-agent/alpha", &base_oid]);
    std::fs::write(work.join("alpha.txt"), "alpha\n").unwrap();
    git(&work, &["add", "alpha.txt"]);
    git(&work, &["commit", "-m", "feat: alpha\n\nTask: T-ALPHA-1"]);
    git(
        &work,
        &["push", "origin", "HEAD:refs/heads/local-agent/alpha"],
    );

    // 2. Clean branch B (divergent from A, touches disjoint file)
    git(&work, &["checkout", "-b", "local-agent/beta", &base_oid]);
    std::fs::write(work.join("beta.txt"), "beta\n").unwrap();
    git(&work, &["add", "beta.txt"]);
    git(&work, &["commit", "-m", "feat: beta\n\nTask: T-BETA-1"]);
    git(
        &work,
        &["push", "origin", "HEAD:refs/heads/local-agent/beta"],
    );

    // 3. Conflicting branch C (modifies alpha.txt differently after alpha merges)
    git(
        &work,
        &["checkout", "-b", "local-agent/conflict", &base_oid],
    );
    std::fs::write(work.join("alpha.txt"), "conflicting-alpha\n").unwrap();
    git(&work, &["add", "alpha.txt"]);
    git(
        &work,
        &["commit", "-m", "feat: conflict\n\nTask: T-CONFLICT-1"],
    );
    git(
        &work,
        &["push", "origin", "HEAD:refs/heads/local-agent/conflict"],
    );

    // 4. Retired branch per T-ADOPT-1.json
    git(&work, &["checkout", "-b", "retired-wave", &base_oid]);
    std::fs::write(work.join("old.txt"), "old\n").unwrap();
    git(&work, &["add", "old.txt"]);
    git(&work, &["commit", "-m", "feat: old\n\nTask: T-OLD-1"]);
    git(&work, &["push", "origin", "HEAD:refs/heads/retired-wave"]);

    // 5. Protected release branch (must NEVER be merged into main!)
    git(
        &work,
        &[
            "checkout",
            "-b",
            "release/v0.21.56-distilled-rc1",
            &base_oid,
        ],
    );
    std::fs::write(work.join("rel.txt"), "rel\n").unwrap();
    git(&work, &["add", "rel.txt"]);
    git(&work, &["commit", "-m", "evidence: rc1\n\nTask: T-REL-1"]);
    git(
        &work,
        &[
            "push",
            "origin",
            "HEAD:refs/heads/release/v0.21.56-distilled-rc1",
        ],
    );

    // 6. Secret-introducing branch (must be refused!)
    git(&work, &["checkout", "-b", "local-agent/leaky", &base_oid]);
    std::fs::write(work.join(".env"), "API_KEY=secret\n").unwrap();
    git(&work, &["add", "-f", ".env"]);
    git(&work, &["commit", "-m", "oops\n\nTask: T-LEAK-1"]);
    git(
        &work,
        &["push", "origin", "HEAD:refs/heads/local-agent/leaky"],
    );

    // 7. Host-committed web merge branch (must be refused unless adopted!)
    git(
        &work,
        &["checkout", "-b", "local-agent/webcommit", &base_oid],
    );
    std::fs::write(work.join("web.txt"), "from web\n").unwrap();
    git(&work, &["add", "web.txt"]);
    let out = Command::new("git")
        .current_dir(&work)
        .args(["commit", "-m", "web edit\n\nTask: T-WEB-1"])
        .env("GIT_AUTHOR_NAME", "Web User")
        .env("GIT_AUTHOR_EMAIL", "user@susi.local")
        .env("GIT_COMMITTER_NAME", "GitHub")
        .env("GIT_COMMITTER_EMAIL", "noreply@github.com")
        .output()
        .unwrap();
    assert!(out.status.success());
    let web_oid = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &["push", "origin", "HEAD:refs/heads/local-agent/webcommit"],
    );

    git(&work, &["checkout", "--detach", &base_oid]);

    let receipt = sweep_pending_branches(&AutoMergeConfig {
        repo: work.clone(),
        remote: "origin".to_string(),
        fetch_remote: true,
        include_local: false,
        ..AutoMergeConfig::default()
    })
    .unwrap();

    assert_eq!(receipt.schema, AUTO_MERGE_RECEIPT_SCHEMA);
    assert_eq!(receipt.stream, "unqualified_development_main");
    assert_eq!(receipt.merged_count, 2);
    assert_eq!(receipt.protected_stream_count, 1);
    assert_eq!(receipt.retired_count, 1);
    assert_eq!(receipt.deferred_or_refused_count, 3);
    assert_eq!(
        receipt.published_remote_main_oid.as_deref(),
        Some(receipt.final_main_oid.as_str())
    );

    let by_name: BTreeMap<&str, BranchSweepDisposition> = receipt
        .branches
        .iter()
        .map(|b| (b.branch.as_str(), b.disposition))
        .collect();
    assert_eq!(
        by_name["local-agent/alpha"],
        BranchSweepDisposition::MergedFastForward
    );
    assert_eq!(
        by_name["local-agent/beta"],
        BranchSweepDisposition::MergedThreeWay
    );
    assert_eq!(
        by_name["local-agent/conflict"],
        BranchSweepDisposition::DeferredConflict
    );
    assert_eq!(
        by_name["retired-wave"],
        BranchSweepDisposition::RetiredByGovernanceLedger
    );
    assert_eq!(
        by_name["release/v0.21.56-distilled-rc1"],
        BranchSweepDisposition::ProtectedStreamBoundary
    );
    assert_eq!(
        by_name["local-agent/leaky"],
        BranchSweepDisposition::ConfidentialityRefused
    );
    assert_eq!(
        by_name["local-agent/webcommit"],
        BranchSweepDisposition::HostCreatedCommitRefused
    );

    // Verify remote origin/main actually has alpha.txt and beta.txt, and NOT rel.txt or .env!
    let remote_main = git(&remote_bare, &["rev-parse", "refs/heads/main"]);
    assert_eq!(remote_main, receipt.final_main_oid);
    let tree_files = git(
        &remote_bare,
        &["ls-tree", "--name-only", "-r", &remote_main],
    );
    assert!(tree_files.lines().any(|l| l == "alpha.txt"));
    assert!(tree_files.lines().any(|l| l == "beta.txt"));
    assert!(!tree_files.lines().any(|l| l == "rel.txt"));
    assert!(!tree_files.lines().any(|l| l == ".env"));

    // Verify commit date on synthesized merge commit is real wall-clock (> 1_700_000_000), never 2026-01-01 or 0!
    let ts_str = git(&remote_bare, &["log", "-1", "--format=%ct", &remote_main]);
    let ts: u64 = ts_str.parse().unwrap();
    assert!(ts > 1_700_000_000, "unexpected committer timestamp {ts}");

    // Adopting the explicit host commit OID permits integrating local-agent/webcommit cleanly!
    let adopted = parse_adopted_oids(&[web_oid]).unwrap();
    let second = sweep_pending_branches(&AutoMergeConfig {
        repo: work,
        remote: "origin".to_string(),
        fetch_remote: true,
        adopted_host_commits: adopted,
        branch_filters: vec!["local-agent/webcommit".to_string()],
        ..AutoMergeConfig::default()
    })
    .unwrap();
    assert_eq!(second.merged_count, 1);
}

#[test]
fn dirty_local_worktree_is_preserved_during_include_local_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    let (_remote_bare, work, base_oid) = setup_storage_and_work(tmp.path());

    let peer_wt = tmp.path().join("peer-wt");
    git(
        &work,
        &[
            "worktree",
            "add",
            "-b",
            "local-agent/peer-active",
            peer_wt.to_str().unwrap(),
            &base_oid,
        ],
    );
    std::fs::write(peer_wt.join("done.txt"), "committed\n").unwrap();
    git(&peer_wt, &["add", "done.txt"]);
    git(
        &peer_wt,
        &["commit", "-m", "feat: part 1\n\nTask: T-PEER-1"],
    );
    // Leave uncommitted work in peer_wt!
    std::fs::write(peer_wt.join("wip.txt"), "still editing\n").unwrap();

    let receipt = sweep_pending_branches(&AutoMergeConfig {
        repo: work,
        remote: "origin".to_string(),
        fetch_remote: true,
        include_local: true,
        ..AutoMergeConfig::default()
    })
    .unwrap();

    assert_eq!(receipt.merged_count, 0);
    let entry = receipt
        .branches
        .iter()
        .find(|b| b.branch == "local-agent/peer-active")
        .expect("found local peer branch");
    assert_eq!(
        entry.disposition,
        BranchSweepDisposition::ActiveWorktreeOrClaim
    );
}

fn push_verified_rc(work: &Path, base_oid: &str, name: &str, task_id: &str) -> String {
    git(work, &["checkout", "--detach", base_oid]);
    let file_name = format!("{name}.txt");
    std::fs::write(work.join(&file_name), format!("payload {name}\n")).unwrap();
    git(work, &["add", &file_name]);
    git(
        work,
        &[
            "commit",
            "-m",
            &format!("Distill task {task_id}\n\nTask: {task_id}"),
        ],
    );
    let final_commit = git(work, &["rev-parse", "HEAD"]);
    let final_tree = git(work, &["rev-parse", "HEAD^{tree}"]);

    let mut rec = json!({
        "schema": "susi-distillation-receipt-v1",
        "name": name,
        "branch_ref": format!("refs/heads/release/{name}"),
        "qualification_status": "accept_verified_candidate",
        "final_commit": final_commit,
        "final_tree": final_tree,
        "included": [{"task_id": task_id}],
        "satisfied_in_base": [],
        "rejected": [],
        "accept_verification": [{"task_id": task_id, "exit_code": 0, "passed": true}]
    });
    let canonical = serde_json::to_vec(&rec).unwrap();
    let sha = susi_paths::hex::encode(Sha256::digest(&canonical).as_slice());
    rec.as_object_mut()
        .unwrap()
        .insert("receipt_sha256".to_string(), Value::String(sha));

    let rel_dir = work.join("releases/distill");
    std::fs::create_dir_all(&rel_dir).unwrap();
    let rel_file = rel_dir.join(format!("{name}.json"));
    std::fs::write(
        &rel_file,
        format!("{}\n", serde_json::to_string_pretty(&rec).unwrap()),
    )
    .unwrap();
    git(work, &["add", &format!("releases/distill/{name}.json")]);
    git(
        work,
        &[
            "commit",
            "-m",
            &format!("evidence(distill): record receipt {name}"),
        ],
    );
    let env_oid = git(work, &["rev-parse", "HEAD"]);
    git(
        work,
        &[
            "push",
            "origin",
            &format!("{env_oid}:refs/heads/release/{name}"),
        ],
    );
    final_commit
}

#[test]
fn promote_release_candidate_ranks_natural_rc_numbers_promotes_verified_envelope_and_refuses_unqualified_main(
) {
    let tmp = tempfile::tempdir().unwrap();
    let (remote_bare, work, base_oid) = setup_storage_and_work(tmp.path());

    let _rc9_final = push_verified_rc(&work, &base_oid, "v0.21.56-distilled-rc9", "T-RC-9");
    let rc11_final = push_verified_rc(&work, &base_oid, "v0.21.56-distilled-rc11", "T-RC-11");

    // Refuses promoting raw unqualified `main` commit onto a release branch!
    let refused = promote_release_candidate(&ReleasePromotionConfig {
        repo: work.clone(),
        remote: "origin".to_string(),
        fetch_remote: true,
        target_release_name: Some("v0.21.56-bad".to_string()),
        candidate_rev: Some(base_oid.clone()),
        dry_run: false,
        receipt_out: None,
    });
    assert!(refused.is_err());

    // Promotes the latest verified candidate (`rc11` > `rc9` by natural numeric order!)
    let promoted = promote_release_candidate(&ReleasePromotionConfig {
        repo: work.clone(),
        remote: "origin".to_string(),
        fetch_remote: true,
        target_release_name: Some("v0.21.56-candidate".to_string()),
        candidate_rev: None,
        dry_run: false,
        receipt_out: None,
    })
    .unwrap();

    assert_eq!(promoted.schema, RELEASE_PROMOTION_RECEIPT_SCHEMA);
    assert_eq!(
        promoted.latest_verified_branch.as_deref(),
        Some("release/v0.21.56-distilled-rc11")
    );
    assert_eq!(
        promoted.promoted_branch_ref.as_deref(),
        Some("refs/heads/release/v0.21.56-candidate")
    );
    assert_eq!(
        promoted.published_snapshot_tag.as_deref(),
        Some("refs/tags/susi-distill/v0.21.56-candidate")
    );
    let pub_oid = promoted.published_remote_oid.clone().unwrap();
    assert_eq!(
        promoted.promoted_commit_oid.as_deref(),
        Some(pub_oid.as_str())
    );

    // Verify local refs/heads/release/v0.21.56-candidate exists locally, remote has ONLY the immutable
    // refs/tags/susi-distill/v0.21.56-candidate tag (never a new remote release/* branch!), and main is untouched!
    assert_eq!(
        git(
            &work,
            &["rev-parse", "refs/heads/release/v0.21.56-candidate"]
        ),
        pub_oid
    );
    assert!(
        Command::new("git")
            .current_dir(&remote_bare)
            .args([
                "rev-parse",
                "--verify",
                "refs/heads/release/v0.21.56-candidate"
            ])
            .output()
            .unwrap()
            .status
            .code()
            != Some(0),
        "remote must NOT receive new refs/heads/release/* branches"
    );
    assert_eq!(
        git(
            &remote_bare,
            &["rev-parse", "refs/tags/susi-distill/v0.21.56-candidate"]
        ),
        pub_oid
    );
    let (verified_parent, _) =
        inspect_distill_envelope(&remote_bare, &pub_oid, Some("v0.21.56-candidate")).unwrap();
    assert_eq!(verified_parent, rc11_final);
    assert_eq!(
        git(&remote_bare, &["rev-parse", "refs/heads/main"]),
        base_oid
    );

    // Re-running with the same target name + same underlying final_commit is idempotent!
    let idempotent = promote_release_candidate(&ReleasePromotionConfig {
        repo: work,
        remote: "origin".to_string(),
        fetch_remote: true,
        target_release_name: Some("v0.21.56-candidate".to_string()),
        candidate_rev: None,
        dry_run: false,
        receipt_out: None,
    })
    .unwrap();
    assert_eq!(
        idempotent.published_remote_oid.as_deref(),
        Some(pub_oid.as_str())
    );
}
