//! Workload completion gates (Swarm OS Bullets 82–89)
//!
//! Each kind names the evidence the kernel requires before a run can be
//! marked complete. The function does not claim a production outcome; it
//! refuses completion when the required receipt, citation, playbook step,
//! or region set is missing.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadKind {
    Engineering,
    VulnerabilityRemediation,
    IncidentTriage,
    ResearchSurvey,
    MonthlyClose,
    ContractReview,
    ContinuousDiscovery,
    MultiSite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadRun {
    pub kind: WorkloadKind,
    pub evidence_ids: Vec<String>,
    pub citation_count: u64,
    pub playbook_steps_completed: u64,
    pub regions: Vec<String>,
}

pub fn complete(run: &WorkloadRun) -> Result<(), String> {
    match run.kind {
        WorkloadKind::Engineering
        | WorkloadKind::MonthlyClose
        | WorkloadKind::ContractReview
        | WorkloadKind::ContinuousDiscovery => require_evidence(run),
        WorkloadKind::VulnerabilityRemediation | WorkloadKind::IncidentTriage => {
            if run.playbook_steps_completed == 0 {
                Err("playbook produced no completed steps".to_string())
            } else {
                require_evidence(run)
            }
        }
        WorkloadKind::ResearchSurvey => {
            if run.citation_count == 0 {
                Err("research survey has no citations".to_string())
            } else {
                require_evidence(run)
            }
        }
        WorkloadKind::MultiSite => {
            if run.regions.len() < 2 {
                Err("multi-site run names fewer than two regions".to_string())
            } else {
                require_evidence(run)
            }
        }
    }
}

fn require_evidence(run: &WorkloadRun) -> Result<(), String> {
    if run.evidence_ids.iter().any(|id| !id.is_empty()) {
        Ok(())
    } else {
        Err("completion requires a non-empty evidence id".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(kind: WorkloadKind) -> WorkloadRun {
        WorkloadRun {
            kind,
            evidence_ids: Vec::new(),
            citation_count: 0,
            playbook_steps_completed: 0,
            regions: Vec::new(),
        }
    }

    #[test]
    fn monthly_close_and_research_and_multi_site_fail_closed_without_their_gates() {
        assert!(complete(&bare(WorkloadKind::MonthlyClose)).is_err());
        let mut close = bare(WorkloadKind::MonthlyClose);
        close.evidence_ids.push("hmac-1".into());
        assert!(complete(&close).is_ok());

        let mut survey = bare(WorkloadKind::ResearchSurvey);
        survey.evidence_ids.push("hmac-1".into());
        assert!(complete(&survey).is_err());
        survey.citation_count = 1;
        assert!(complete(&survey).is_ok());

        let mut sites = bare(WorkloadKind::MultiSite);
        sites.evidence_ids.push("hmac-1".into());
        sites.regions = vec!["eu".into()];
        assert!(complete(&sites).is_err());
        sites.regions.push("us".into());
        assert!(complete(&sites).is_ok());
    }
}
