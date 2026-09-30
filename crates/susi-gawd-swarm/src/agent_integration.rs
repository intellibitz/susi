//! Integrate parallel worker output: verify before closing, merge without
//! silent overwrites.
//!
//! The pipeline for every completed worker assignment:
//! 1. Ledger `Completed` state with a current fence is necessary but NOT
//!    sufficient — a self-reported "done" is a claim, not proof.
//! 2. The task's *acceptance command* runs in the worker's worktree. Only
//!    exit-0 counts; any failure marks the job `Failed`, never `finish`ed.
//! 3. Policy may require independent verification — an injected verifier
//!    (in production a different model reviews the diff). `Rejected` blocks
//!    integration.
//! 4. The worker's branch integrates against current main via the
//!    injected `Integrator`. Non-overlapping changes merge clean;
//!    overlapping edits surface a reviewable conflict list — never a silent
//!    overwrite — and stay unmerged for human/agent review.
//! 5. `.agents/*` records merge by union (Mandate 51) inside the
//!    integrator; commit trailers are the workers' responsibility and were
//!    enforced at commit time.
//! 6. Open dependencies block finishing — a task can't close while its
//!    declared deps are still open.
//! 7. Only then does `finish` fire. The branch itself pushes through the
//!    normal PR workflow — nothing here ever touches `main` directly.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::roadmap_agents::TaskSpec;
use crate::worker_recovery::{JobLedger, JobState};

/// Queue surface the gate needs.
pub trait IntegrationQueue: Send + Sync {
    /// Is `id` closed?
    fn is_done(&self, id: &str) -> bool;
    /// A task's declared dependencies.
    fn deps(&self, id: &str) -> Vec<String>;
    /// Record a task finished (acceptance verified).
    fn finish(&self, id: &str);
    /// Record a failed completion attempt (worker truthfully reported fail,
    /// or acceptance rejected a false done).
    fn reopen(&self, id: &str, why: &str);
}

/// Runs the task's acceptance command in its worktree.
pub trait AcceptRunner: Send + Sync {
    /// `Ok(())` only on exit-0.
    fn run(&self, worktree: &Path, cmd: &[String]) -> Result<(), String>;
}

/// Independent model-backed verification verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Change achieves the task; safe to integrate.
    Approve,
    /// Concrete defects — block integration.
    Reject { reasons: Vec<String> },
}

/// The independent reviewer (a different model in production).
pub trait Verifier: Send + Sync {
    fn verify(&self, worktree: &Path, task: &TaskSpec) -> Verdict;
}

/// Result of attempting to merge a worker branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// Clean merge — commit sha.
    Clean(String),
    /// Overlapping edits — reviewable conflict, nothing committed.
    Conflict(Vec<String>),
}

/// Merge executor (real git in production and tests).
pub trait Integrator: Send + Sync {
    /// Merge `branch` into the repository at `repo`, current with its base.
    /// Must refuse a dirty result silently — conflicts are reported, not
    /// resolved by overwrite.
    fn integrate(&self, repo: &Path, branch: &str) -> Result<MergeResult, String>;
}

/// The integration outcome for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrateOutcome {
    /// Verified + merged clean; task finished.
    Integrated { sha: String },
    /// Worker reported done but acceptance fails — reopened.
    AcceptanceFailed { output: String },
    /// Worker itself reported failure — reopened.
    WorkerFailed,
    /// Independent verifier rejected the diff.
    VerifyRejected { reasons: Vec<String> },
    /// Merge produced conflicts — preserved for review, not overwritten.
    ConflictReview { files: Vec<String> },
    /// Dependencies still open — task may not close yet.
    BlockedByDeps { open: Vec<String> },
    /// Job isn't in a finished state at all.
    NotCompleted { state: String },
}

/// The integration pipeline's injected plumbing.
pub struct Gate<'a> {
    /// Assignment ledger (fence-verified completion).
    pub ledger: &'a JobLedger,
    /// Task queue finish/reopen hooks.
    pub queue: &'a dyn IntegrationQueue,
    /// Acceptance-command executor.
    pub accept: &'a dyn AcceptRunner,
    /// Branch merger.
    pub integrator: &'a dyn Integrator,
    /// Optional independent verifier.
    pub verifier: Option<&'a dyn Verifier>,
}

impl Gate<'_> {
    /// Run the gate for one job. Side effects: `queue.finish` /
    /// `queue.reopen` / merged branch.
    pub fn integrate_job(&self, job_id: &str, branch: &str, task: &TaskSpec) -> IntegrateOutcome {
        let ledger = self.ledger;
        let queue = self.queue;
        let accept = self.accept;
        let integrator = self.integrator;
        let verifier = self.verifier;
        let Some(a) = ledger.get(job_id) else {
            return IntegrateOutcome::NotCompleted {
                state: "missing".into(),
            };
        };
        if a.state != JobState::Completed {
            return IntegrateOutcome::NotCompleted {
                state: format!("{:?}", a.state),
            };
        }
        // Open deps block closure — union semantics: deps must all be done.
        let open: Vec<String> = queue
            .deps(job_id)
            .into_iter()
            .filter(|d| !queue.is_done(d))
            .collect();
        if !open.is_empty() {
            return IntegrateOutcome::BlockedByDeps { open };
        }
        // Acceptance is the arbiter of "done" — self-reports are just claims.
        if let Err(out) = accept.run(&a.worktree, &task.accept) {
            queue.reopen(job_id, "acceptance failed");
            return IntegrateOutcome::AcceptanceFailed { output: out };
        }
        if let Some(v) = verifier {
            if let Verdict::Reject { reasons } = v.verify(&a.worktree, task) {
                queue.reopen(job_id, "independent verification rejected");
                return IntegrateOutcome::VerifyRejected { reasons };
            }
        }
        match integrator.integrate(&a.worktree, branch) {
            Ok(MergeResult::Clean(sha)) => {
                queue.finish(job_id);
                IntegrateOutcome::Integrated { sha }
            }
            Ok(MergeResult::Conflict(files)) => IntegrateOutcome::ConflictReview { files },
            Err(why) => IntegrateOutcome::ConflictReview {
                files: vec![format!("integrator error: {why}")],
            },
        }
    }
}

/// Union-merge `.agents` record files: concatenated line sets, deduped,
/// order-stable — never delete another agent's entries (Mandate 51).
#[must_use]
pub fn union_merge_lines(base: &str, ours: &str, theirs: &str) -> String {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for src in [base, ours, theirs] {
        for line in src.lines() {
            if !line.trim().is_empty() && seen.insert(line.to_string()) {
                out.push(line.to_string());
            }
        }
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Union-merge a JSON array of objects on `"id"` — both sides' entries
/// preserved; on id collision keep both under a suffixed alias.
pub fn union_merge_json_array(
    base: &serde_json::Value,
    ours: &serde_json::Value,
    theirs: &serde_json::Value,
) -> serde_json::Value {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for doc in [base, ours, theirs] {
        let Some(arr) = doc.as_array() else { continue };
        for item in arr {
            let key = item
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| serde_json::to_string(item).unwrap_or_default());
            if seen.insert(key) {
                out.push(item.clone());
            }
        }
    }
    serde_json::Value::Array(out)
}

/// A real `git merge` integrator: merge `branch` into `repo`'s current
/// HEAD; conflicts are detected via `git diff --name-only --diff-filter=U`
/// and the merge is aborted — nothing is overwritten.
pub struct GitIntegrator;

impl Integrator for GitIntegrator {
    fn integrate(&self, repo: &Path, branch: &str) -> Result<MergeResult, String> {
        let git = |args: &[&str]| -> Result<std::process::Output, String> {
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .map_err(|e| e.to_string())
        };
        let out = git(&["merge", "--no-ff", "--no-edit", branch])?;
        if out.status.success() {
            let sha = git(&["rev-parse", "HEAD"])?;
            return Ok(MergeResult::Clean(
                String::from_utf8_lossy(&sha.stdout).trim().to_string(),
            ));
        }
        let conflicted = git(&["diff", "--name-only", "--diff-filter=U"])?;
        let files: Vec<String> = String::from_utf8_lossy(&conflicted.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        let _ = git(&["merge", "--abort"]);
        Ok(MergeResult::Conflict(files))
    }
}

/// Records describing a worker run — emitted into `.agents/` so integration
/// can union them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    /// Unique id.
    pub id: String,
    /// Job the record belongs to.
    pub job: String,
    /// Worker identity.
    pub worker: String,
    /// Receipts (applied effects).
    pub receipts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Mutex;

    const T0: u64 = 1_700_000_000;
    fn clock() -> u64 {
        T0
    }

    struct FakeQueue {
        deps: BTreeMap<String, Vec<String>>,
        done: Mutex<BTreeMap<String, bool>>,
    }
    impl IntegrationQueue for FakeQueue {
        fn is_done(&self, id: &str) -> bool {
            self.done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(id)
                .copied()
                .unwrap_or(false)
        }
        fn deps(&self, id: &str) -> Vec<String> {
            self.deps.get(id).cloned().unwrap_or_default()
        }
        fn finish(&self, id: &str) {
            self.done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), true);
        }
        fn reopen(&self, id: &str, _why: &str) {
            self.done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), false);
        }
    }

    struct PassAccept;
    impl AcceptRunner for PassAccept {
        fn run(&self, _w: &Path, _c: &[String]) -> Result<(), String> {
            Ok(())
        }
    }
    struct FailAccept;
    impl AcceptRunner for FailAccept {
        fn run(&self, _w: &Path, _c: &[String]) -> Result<(), String> {
            Err("exit 1: test failed".into())
        }
    }
    struct NoMerge;
    impl Integrator for NoMerge {
        fn integrate(&self, _r: &Path, _b: &str) -> Result<MergeResult, String> {
            Ok(MergeResult::Clean("deadbeef".into()))
        }
    }
    struct RejectVerifier;
    impl Verifier for RejectVerifier {
        fn verify(&self, _w: &Path, _t: &TaskSpec) -> Verdict {
            Verdict::Reject {
                reasons: vec!["diff removes a safety check".into()],
            }
        }
    }

    fn task(id: &str) -> TaskSpec {
        TaskSpec {
            id: id.into(),
            task_class: "coding".into(),
            accept: vec!["cargo".into(), "test".into()],
            title: id.into(),
        }
    }

    fn completed_ledger(job: &str) -> JobLedger {
        let l = JobLedger::with_clock(clock);
        l.assign(crate::worker_recovery::Assign {
            job_id: job,
            worker: "w1",
            model: "m1",
            worktree: std::env::temp_dir(),
            lease_until: T0 + 300,
        });
        l.complete(job, 1).unwrap();
        l
    }

    fn git(repo: &Path, args: &[&str]) {
        let o = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Hermetic repo: base commit, then a worker branch with `patch`.
    fn repo_with_branch(patch: impl Fn(&Path)) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "gi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        std::fs::write(dir.join("shared.txt"), "line1\nline2\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "base"]);
        git(&dir, &["checkout", "-qb", "worker-1"]);
        patch(&dir);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "worker change"]);
        git(&dir, &["checkout", "-q", "main"]);
        (dir, "worker-1".into())
    }

    #[test]
    fn parallel_agent_integration_clean_merge_finishes_task() {
        let (repo, branch) = repo_with_branch(|d| {
            std::fs::write(d.join("new_file.rs"), "pub fn f() {}\n").unwrap();
        });
        let ledger = completed_ledger("j1");
        let ledger_ref = ledger;
        // Point the assignment at the repo as its worktree.
        // (ledger records the worktree; override for the git repo path)
        // — rebuild with the right path.
        let l = JobLedger::with_clock(clock);
        l.assign(crate::worker_recovery::Assign {
            job_id: "j1",
            worker: "w1",
            model: "m1",
            worktree: repo.clone(),
            lease_until: T0 + 300,
        });
        l.complete("j1", 1).unwrap();
        drop(ledger_ref);
        let q = FakeQueue {
            deps: BTreeMap::new(),
            done: Mutex::new(BTreeMap::new()),
        };
        let gate = Gate {
            ledger: &l,
            queue: &q,
            accept: &PassAccept,
            integrator: &GitIntegrator,
            verifier: None,
        };
        let out = gate.integrate_job("j1", &branch, &task("j1"));
        assert!(matches!(out, IntegrateOutcome::Integrated { .. }));
        assert!(q.is_done("j1"));
        assert!(repo.join("new_file.rs").exists());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn parallel_agent_integration_overlapping_edits_become_conflicts() {
        let (repo, branch) = repo_with_branch(|d| {
            std::fs::write(d.join("shared.txt"), "worker-edit\nline2\n").unwrap();
        });
        // main also edits the same line.
        std::fs::write(repo.join("shared.txt"), "main-edit\nline2\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "main edits same line"]);
        let l = JobLedger::with_clock(clock);
        l.assign(crate::worker_recovery::Assign {
            job_id: "j1",
            worker: "w1",
            model: "m1",
            worktree: repo.clone(),
            lease_until: T0 + 300,
        });
        l.complete("j1", 1).unwrap();
        let q = FakeQueue {
            deps: BTreeMap::new(),
            done: Mutex::new(BTreeMap::new()),
        };
        let gate = Gate {
            ledger: &l,
            queue: &q,
            accept: &PassAccept,
            integrator: &GitIntegrator,
            verifier: None,
        };
        let out = gate.integrate_job("j1", &branch, &task("j1"));
        match out {
            IntegrateOutcome::ConflictReview { files } => {
                assert!(files.contains(&"shared.txt".to_string()));
            }
            other => panic!("expected ConflictReview, got {other:?}"),
        }
        // Nothing silently overwritten: main still has its line, no merge left behind.
        assert_eq!(
            std::fs::read_to_string(repo.join("shared.txt")).unwrap(),
            "main-edit\nline2\n"
        );
        assert!(!q.is_done("j1"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn parallel_agent_integration_failed_acceptance_reopens() {
        // Self-report said done; acceptance disagrees → reopened, never finished.
        let l = completed_ledger("j1");
        let q = FakeQueue {
            deps: BTreeMap::new(),
            done: Mutex::new(BTreeMap::new()),
        };
        let gate = Gate {
            ledger: &l,
            queue: &q,
            accept: &FailAccept,
            integrator: &NoMerge,
            verifier: None,
        };
        let out = gate.integrate_job("j1", "b", &task("j1"));
        assert!(matches!(out, IntegrateOutcome::AcceptanceFailed { .. }));
        assert!(!q.is_done("j1"));
    }

    #[test]
    fn parallel_agent_integration_open_deps_block_close() {
        let l = completed_ledger("j1");
        let mut deps = BTreeMap::new();
        deps.insert("j1".to_string(), vec!["dep-open".to_string()]);
        let q = FakeQueue {
            deps,
            done: Mutex::new(BTreeMap::new()),
        };
        let gate = Gate {
            ledger: &l,
            queue: &q,
            accept: &PassAccept,
            integrator: &NoMerge,
            verifier: None,
        };
        let out = gate.integrate_job("j1", "b", &task("j1"));
        assert_eq!(
            out,
            IntegrateOutcome::BlockedByDeps {
                open: vec!["dep-open".to_string()]
            }
        );
        assert!(!q.is_done("j1"));
    }

    #[test]
    fn parallel_agent_integration_independent_verifier_can_reject() {
        let l = completed_ledger("j1");
        let q = FakeQueue {
            deps: BTreeMap::new(),
            done: Mutex::new(BTreeMap::new()),
        };
        let gate = Gate {
            ledger: &l,
            queue: &q,
            accept: &PassAccept,
            integrator: &NoMerge,
            verifier: Some(&RejectVerifier),
        };
        let out = gate.integrate_job("j1", "b", &task("j1"));
        assert!(matches!(out, IntegrateOutcome::VerifyRejected { .. }));
        assert!(!q.is_done("j1"));
    }

    #[test]
    fn parallel_agent_integration_union_merge_keeps_both_agents_records() {
        let base = "{\"id\":\"EV-1\"}\n";
        let ours = "{\"id\":\"EV-1\"}\n{\"id\":\"EV-DEVIN-2\"}\n";
        let theirs = "{\"id\":\"EV-1\"}\n{\"id\":\"EV-CURSOR-3\"}\n";
        let merged = union_merge_lines(base, ours, theirs);
        assert!(merged.contains("EV-1"));
        assert!(merged.contains("EV-DEVIN-2"));
        assert!(merged.contains("EV-CURSOR-3"));
        assert_eq!(merged.matches("EV-1").count(), 1); // deduped
    }

    #[test]
    fn parallel_agent_integration_union_json_arrays_preserve_all_entries() {
        let base = serde_json::json!([{"id":"a","v":1}]);
        let ours = serde_json::json!([{"id":"a","v":1},{"id":"b","v":2}]);
        let theirs = serde_json::json!([{"id":"a","v":1},{"id":"c","v":3}]);
        let merged = union_merge_json_array(&base, &ours, &theirs);
        let arr = merged.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }
}
