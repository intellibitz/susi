//! Scheduled missions with evidence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMission {
    pub id: String,
    pub cron: String,
    pub evidence_required: bool,
}

/// Build a scheduled mission; evidence is always required for COMPLETE.
#[must_use]
pub fn schedule(id: &str, cron: &str) -> ScheduledMission {
    ScheduledMission {
        id: id.into(),
        cron: cron.into(),
        evidence_required: true,
    }
}

#[must_use]
pub fn due_now(cron: &str, minute: u32) -> bool {
    if let Some(rest) = cron.strip_prefix("*/") {
        let n: u32 = rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        return n > 0 && minute % n == 0;
    }
    false
}

#[cfg(test)]
mod scheduled_missions_tests {
    use super::*;

    #[test]
    fn scheduled_missions_require_evidence() {
        let m = schedule("nightly", "*/15 * * * *");
        assert!(m.evidence_required);
        assert!(due_now(&m.cron, 30));
        assert!(!due_now(&m.cron, 31));
    }
}
