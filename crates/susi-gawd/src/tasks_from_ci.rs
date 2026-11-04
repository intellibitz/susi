//! Auto-create tasks from red CI / failing e2e checks.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailure {
    pub job: String,
    pub check: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCiTask {
    pub title: String,
    pub accept: String,
}

#[must_use]
pub fn tasks_from_ci(failures: &[CiFailure]) -> Vec<DraftCiTask> {
    let mut out = Vec::new();
    for f in failures {
        out.push(DraftCiTask {
            title: format!("Fix red CI: {} / {}", f.job, f.check),
            accept: if f.check.starts_with("cargo ") {
                f.check.clone()
            } else {
                format!("cargo test --test {}", f.check)
            },
        });
    }
    out
}

#[cfg(test)]
mod tasks_from_ci_tests {
    use super::*;

    #[test]
    fn tasks_from_ci_opens_for_each_failure() {
        let drafts = tasks_from_ci(&[
            CiFailure {
                job: "Test".into(),
                check: "workflow_compliance".into(),
                message: "red".into(),
            },
            CiFailure {
                job: "e2e".into(),
                check: "cargo test -p susi-gawd vc_201_001".into(),
                message: "fail".into(),
            },
        ]);
        assert_eq!(drafts.len(), 2);
        assert!(drafts[0].accept.contains("workflow_compliance"));
        assert!(drafts[1].accept.starts_with("cargo test"));
    }

    #[test]
    fn tasks_from_ci_empty_slice_returns_empty_vec() {
        let drafts = tasks_from_ci(&[]);
        assert!(drafts.is_empty());
    }

    #[test]
    fn tasks_from_ci_non_cargo_check_formats_integration_test_command() {
        let drafts = tasks_from_ci(&[CiFailure {
            job: "Unit".into(),
            check: "repo_invariants".into(),
            message: "failed".into(),
        }]);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].accept, "cargo test --test repo_invariants");
    }

    #[test]
    fn tasks_from_ci_preserves_order_and_formats_title_with_job_and_check() {
        let drafts = tasks_from_ci(&[
            CiFailure {
                job: "JobA".into(),
                check: "check_one".into(),
                message: "m1".into(),
            },
            CiFailure {
                job: "JobB".into(),
                check: "cargo clippy --all".into(),
                message: "m2".into(),
            },
        ]);
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].title, "Fix red CI: JobA / check_one");
        assert_eq!(drafts[1].title, "Fix red CI: JobB / cargo clippy --all");
        assert_eq!(drafts[1].accept, "cargo clippy --all");
    }

    #[test]
    fn tasks_from_ci_passes_through_direct_cargo_test_check_as_accept_command() {
        let drafts = tasks_from_ci(&[CiFailure {
            job: "Linux".into(),
            check: "cargo test --workspace".into(),
            message: "assertion failed".into(),
        }]);
        assert_eq!(drafts.len(), 1);
        assert_eq!(
            drafts[0].title,
            "Fix red CI: Linux / cargo test --workspace"
        );
        assert_eq!(drafts[0].accept, "cargo test --workspace");
    }
}
