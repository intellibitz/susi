//! Local web dashboard for ecosystem, brain and tasks.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardPage {
    pub path: String,
    pub title: String,
}

/// Routes served by the local dashboard.
#[must_use]
pub fn dashboard_pages() -> Vec<DashboardPage> {
    [
        ("/", "Overview"),
        ("/ecosystem", "Ecosystem"),
        ("/brain", "Brain"),
        ("/tasks", "Tasks"),
    ]
    .into_iter()
    .map(|(path, title)| DashboardPage {
        path: path.into(),
        title: title.into(),
    })
    .collect()
}

#[cfg(test)]
mod web_dashboard_tests {
    use super::*;

    #[test]
    fn web_dashboard_lists_core_pages() {
        let pages = dashboard_pages();
        assert!(pages.iter().any(|p| p.path == "/ecosystem"));
        assert!(pages.iter().any(|p| p.path == "/brain"));
        assert!(pages.iter().any(|p| p.path == "/tasks"));
    }
}
