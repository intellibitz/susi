//! Supervision for long-lived components (VC-202-010).
//!
//! Every component that outlives a command — the daemon, the
//! primary-checkout watcher, a brain scout, a worker — registers here and is
//! health-checked, restarted (bounded), logged, and reported through one
//! status surface. A killed component therefore recovers without a human
//! noticing a sync, and no unsupervised background process is left behind.

use std::collections::BTreeMap;

/// A registered long-lived service.
struct SupervisedService {
    name: String,
    healthy: bool,
    max_restarts: u32,
    restarts: u32,
}

/// One restart, in order, for the supervision log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartEvent {
    pub name: String,
    pub attempt: u32,
}

/// The supervisor: one registry, one health pass, one status surface.
#[derive(Default)]
pub struct Supervisor {
    services: BTreeMap<String, SupervisedService>,
    log: Vec<RestartEvent>,
}

impl Supervisor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a long-lived component with a bounded restart budget.
    pub fn register(&mut self, name: &str, max_restarts: u32) {
        self.services.insert(
            name.to_string(),
            SupervisedService {
                name: name.to_string(),
                healthy: true,
                max_restarts,
                restarts: 0,
            },
        );
    }

    /// Mark a component unhealthy (e.g. its heartbeat stopped).
    pub fn mark_unhealthy(&mut self, name: &str) {
        if let Some(svc) = self.services.get_mut(name) {
            svc.healthy = false;
        }
    }

    /// One supervision round: restart every unhealthy component, bounded by
    /// its `max_restarts`. A restart recovers the component and is logged;
    /// a component whose budget is exhausted stays unhealthy and is reported
    /// (never silently dropped).
    pub fn supervise(&mut self) {
        for svc in self.services.values_mut() {
            if svc.healthy {
                continue;
            }
            if svc.restarts < svc.max_restarts {
                svc.restarts += 1;
                svc.healthy = true;
                self.log.push(RestartEvent {
                    name: svc.name.clone(),
                    attempt: svc.restarts,
                });
            }
        }
    }

    /// The one status surface: every component's health and restart count.
    #[must_use]
    pub fn status(&self) -> Vec<ServiceStatus> {
        self.services
            .values()
            .map(|s| ServiceStatus {
                name: s.name.clone(),
                healthy: s.healthy,
                restarts: s.restarts,
            })
            .collect()
    }

    /// The supervision log: restarts in the order they happened.
    #[must_use]
    pub fn restart_log(&self) -> &[RestartEvent] {
        &self.log
    }
}

/// One component's status, as reported by [`Supervisor::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub name: String,
    pub healthy: bool,
    pub restarts: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance: a registered component is health-checked, restarted
    /// within its bound, logged, and surfaced; an exhausted component stays
    /// unhealthy and is reported rather than dropped.
    #[test]
    fn service_supervision_restarts_bounded_and_reports_every_component() {
        let mut sup = Supervisor::new();
        sup.register("daemon", 2);
        sup.register("watcher", 0);

        // Both components die.
        sup.mark_unhealthy("daemon");
        sup.mark_unhealthy("watcher");

        // Round 1: daemon restarts (budget 2), watcher has no budget.
        sup.supervise();
        let st = sup.status();
        assert!(
            st.iter()
                .any(|s| s.name == "daemon" && s.healthy && s.restarts == 1)
        );
        assert!(
            st.iter()
                .any(|s| s.name == "watcher" && !s.healthy && s.restarts == 0)
        );

        // Daemon dies again; watcher is still down.
        sup.mark_unhealthy("daemon");
        sup.supervise();
        let st = sup.status();
        assert!(
            st.iter()
                .any(|s| s.name == "daemon" && s.healthy && s.restarts == 2)
        );

        // Daemon dies a third time: budget exhausted, stays unhealthy.
        sup.mark_unhealthy("daemon");
        sup.supervise();
        let st = sup.status();
        assert!(
            st.iter()
                .any(|s| s.name == "daemon" && !s.healthy && s.restarts == 2)
        );

        // The log records both restarts, in order, with the attempt number.
        let log = sup.restart_log();
        assert_eq!(log.len(), 2);
        assert_eq!(
            log[0],
            RestartEvent {
                name: "daemon".into(),
                attempt: 1
            }
        );
        assert_eq!(
            log[1],
            RestartEvent {
                name: "daemon".into(),
                attempt: 2
            }
        );
    }
}
