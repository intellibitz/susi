//! Inter-cell OS Signals (Swarm OS Bullet 12)
//!
//! Supports sending native OS-like signals (SIGKILL, SIGSTOP, SIGCONT,
//! SIGTERM) between cells. Tracks each targeted cell's resulting run
//! state so a dispatched signal has a real, queryable effect rather than
//! being logged and discarded.

use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSignal {
    SigKill,
    SigStop,
    SigCont,
    SigTerm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellRunState {
    Running,
    Stopped,
    Terminated,
}

pub struct SignalRouter {
    states: RwLock<HashMap<String, CellRunState>>,
}

impl Default for SignalRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalRouter {
    pub fn new() -> Self {
        Self {
            states: RwLock::new(HashMap::new()),
        }
    }

    /// Applies `signal`'s state transition to `target_cell`. A cell that's
    /// already `Terminated` rejects further signals — a killed cell
    /// doesn't come back from `SigCont`.
    pub fn dispatch_signal(&self, target_cell: &str, signal: CellSignal) -> Result<(), String> {
        let mut states = self.states.write().unwrap_or_else(|e| e.into_inner());
        if states.get(target_cell) == Some(&CellRunState::Terminated) {
            return Err(format!("cell '{target_cell}' is already terminated"));
        }
        let next = match signal {
            CellSignal::SigKill | CellSignal::SigTerm => CellRunState::Terminated,
            CellSignal::SigStop => CellRunState::Stopped,
            CellSignal::SigCont => CellRunState::Running,
        };
        states.insert(target_cell.to_string(), next);
        Ok(())
    }

    pub fn run_state(&self, cell_id: &str) -> Option<CellRunState> {
        self.states
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cell_id)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigkill_terminates_and_further_signals_are_rejected() {
        let router = SignalRouter::new();
        router
            .dispatch_signal("cell-a", CellSignal::SigKill)
            .unwrap();
        assert_eq!(router.run_state("cell-a"), Some(CellRunState::Terminated));
        assert!(
            router
                .dispatch_signal("cell-a", CellSignal::SigCont)
                .is_err()
        );
    }

    #[test]
    fn sigstop_then_sigcont_round_trips() {
        let router = SignalRouter::new();
        router
            .dispatch_signal("cell-a", CellSignal::SigStop)
            .unwrap();
        assert_eq!(router.run_state("cell-a"), Some(CellRunState::Stopped));
        router
            .dispatch_signal("cell-a", CellSignal::SigCont)
            .unwrap();
        assert_eq!(router.run_state("cell-a"), Some(CellRunState::Running));
    }

    #[test]
    fn unsignaled_cell_has_no_recorded_state() {
        let router = SignalRouter::new();
        assert_eq!(router.run_state("never-signaled"), None);
    }
}
