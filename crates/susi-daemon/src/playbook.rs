//! Incident playbooks (Swarm OS Bullet 60)
//!
//! Steps run in order. The first step whose capability is not granted
//! stops the run. Completed steps are returned either way; a stop is not
//! reported as a finished incident.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub action: String,
    pub requires_capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playbook {
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybookOutcome {
    pub completed: Vec<String>,
    pub stopped_at: Option<String>,
}

pub fn execute(playbook: &Playbook, granted: &[String]) -> PlaybookOutcome {
    let mut completed = Vec::new();
    for step in &playbook.steps {
        if !granted.iter().any(|cap| cap == &step.requires_capability) {
            return PlaybookOutcome {
                completed,
                stopped_at: Some(step.action.clone()),
            };
        }
        completed.push(step.action.clone());
    }
    PlaybookOutcome {
        completed,
        stopped_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_stops_at_the_first_missing_capability() {
        let playbook = Playbook {
            name: "triage".into(),
            steps: vec![
                Step {
                    action: "snapshot".into(),
                    requires_capability: "incident:read".into(),
                },
                Step {
                    action: "isolate".into(),
                    requires_capability: "incident:isolate".into(),
                },
            ],
        };
        let outcome = execute(&playbook, &["incident:read".into()]);
        assert_eq!(outcome.completed, vec!["snapshot".to_string()]);
        assert_eq!(outcome.stopped_at.as_deref(), Some("isolate"));
    }
}
