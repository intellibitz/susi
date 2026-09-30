//! Key age tracking and rotation reminders.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyAge {
    pub vendor: String,
    pub created_unix: u64,
    pub last_used_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RotationReminder {
    None,
    Soon { days_left: u32 },
    Overdue { days_over: u32 },
}

/// Remind when a key is older than `max_age_days` (default rotation window).
#[must_use]
pub fn reminder(age: &KeyAge, now_unix: u64, max_age_days: u32) -> RotationReminder {
    let age_secs = now_unix.saturating_sub(age.created_unix);
    let age_days = (age_secs / 86_400) as u32;
    if age_days >= max_age_days {
        return RotationReminder::Overdue {
            days_over: age_days.saturating_sub(max_age_days),
        };
    }
    let days_left = max_age_days.saturating_sub(age_days);
    if days_left <= 14 {
        return RotationReminder::Soon { days_left };
    }
    RotationReminder::None
}

#[cfg(test)]
mod key_rotation_tests {
    use super::*;

    #[test]
    fn key_rotation_reminds_when_aging() {
        let k = KeyAge {
            vendor: "openai".into(),
            created_unix: 0,
            last_used_unix: 0,
        };
        assert!(matches!(
            reminder(&k, 86_400 * 100, 90),
            RotationReminder::Overdue { .. }
        ));
        assert!(matches!(
            reminder(&k, 86_400 * 80, 90),
            RotationReminder::Soon { days_left: 10 }
        ));
        assert!(matches!(
            reminder(&k, 86_400 * 10, 90),
            RotationReminder::None
        ));
    }
}
