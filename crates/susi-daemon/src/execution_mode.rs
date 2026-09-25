//! Execution modes (Swarm OS Bullet 16)
//!
//! Reactive cells run on an event, proactive cells run on a schedule,
//! continuous cells run on a stream. A trigger wakes only the mode it
//! names.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Reactive,
    Proactive,
    Continuous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Event,
    Schedule,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeBinding {
    pub cell_id: String,
    pub mode: ExecutionMode,
}

pub fn woken_by(bindings: &[ModeBinding], trigger: Trigger) -> Vec<String> {
    bindings
        .iter()
        .filter(|binding| matches_trigger(binding.mode, trigger))
        .map(|binding| binding.cell_id.clone())
        .collect()
}

fn matches_trigger(mode: ExecutionMode, trigger: Trigger) -> bool {
    match (mode, trigger) {
        (ExecutionMode::Reactive, Trigger::Event)
        | (ExecutionMode::Proactive, Trigger::Schedule)
        | (ExecutionMode::Continuous, Trigger::Stream) => true,
        (ExecutionMode::Reactive, Trigger::Schedule | Trigger::Stream)
        | (ExecutionMode::Proactive, Trigger::Event | Trigger::Stream)
        | (ExecutionMode::Continuous, Trigger::Event | Trigger::Schedule) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schedule_wakes_only_proactive_cells() {
        let bindings = vec![
            ModeBinding {
                cell_id: "react".into(),
                mode: ExecutionMode::Reactive,
            },
            ModeBinding {
                cell_id: "cron".into(),
                mode: ExecutionMode::Proactive,
            },
            ModeBinding {
                cell_id: "stream".into(),
                mode: ExecutionMode::Continuous,
            },
        ];
        assert_eq!(
            woken_by(&bindings, Trigger::Schedule),
            vec!["cron".to_string()]
        );
        assert_eq!(
            woken_by(&bindings, Trigger::Event),
            vec!["react".to_string()]
        );
        assert_eq!(
            woken_by(&bindings, Trigger::Stream),
            vec!["stream".to_string()]
        );
    }
}
