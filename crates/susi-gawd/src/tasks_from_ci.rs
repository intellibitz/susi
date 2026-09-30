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
}
