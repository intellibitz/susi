//! Playground (Swarm OS Bullet 68)
//!
//! Composes scaffolded cells and runs them against one synthetic task.
//! The run is the simulation from Bullet 65, not a graphical canvas.

use crate::scaffold::{self, AgentTemplate};
use crate::simulation::{self, SimResult, SyntheticTask};

pub fn run_sample() -> Vec<SimResult> {
    let coder = scaffold::scaffold(AgentTemplate::Coder, "coder");
    let tester = scaffold::scaffold(AgentTemplate::Tester, "tester");
    let task = SyntheticTask {
        name: "sample-test".into(),
        capability_hash: scaffold::capability_hash("code:test"),
        tokens: 8,
    };
    simulation::run(&[coder, tester], &[task])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_task_is_assigned_to_the_tester_template() {
        let results = run_sample();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].assigned_cell.as_deref(), Some("tester"));
        assert_eq!(results[0].tokens, 8);
    }
}
