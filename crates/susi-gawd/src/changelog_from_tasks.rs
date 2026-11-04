//! Changelog / release notes from closed tasks since last tag.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedTask {
    pub id: String,
    pub title: String,
    pub closed_unix: u64,
}

/// Build markdown changelog from tasks closed after `since_unix` (last tag time).
#[must_use]
pub fn changelog_from_tasks(closed: &[ClosedTask], since_unix: u64) -> String {
    let mut items: Vec<_> = closed
        .iter()
        .filter(|t| t.closed_unix > since_unix)
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let mut out = String::from("## Changes\n\n");
    if items.is_empty() {
        out.push_str("_No closed tasks since last tag._\n");
        return out;
    }
    for t in items {
        out.push_str(&format!("- {}: {}\n", t.id, t.title));
    }
    out
}

#[cfg(test)]
mod changelog_from_tasks_tests {
    use super::*;

    #[test]
    fn changelog_from_tasks_lists_since_last_tag() {
        let closed = vec![
            ClosedTask {
                id: "T-CLAUDE-2".into(),
                title: "later".into(),
                closed_unix: 200,
            },
            ClosedTask {
                id: "T-CLAUDE-1".into(),
                title: "earlier".into(),
                closed_unix: 50,
            },
        ];
        let md = changelog_from_tasks(&closed, 100);
        assert!(md.contains("T-CLAUDE-2"));
        assert!(!md.contains("T-CLAUDE-1"));
    }

    #[test]
    fn changelog_from_tasks_empty_note() {
        assert!(changelog_from_tasks(&[], 0).contains("No closed tasks"));
    }

    #[test]
    fn changelog_from_tasks_excludes_task_strictly_equal_to_since_unix() {
        let closed = vec![ClosedTask {
            id: "T-BOUNDARY".into(),
            title: "exact tag timestamp".into(),
            closed_unix: 500,
        }];
        let md = changelog_from_tasks(&closed, 500);
        assert!(md.contains("No closed tasks"));
        assert!(!md.contains("T-BOUNDARY"));
    }

    #[test]
    fn changelog_from_tasks_renders_sorted_bullets_for_multiple_matching_tasks() {
        let closed = vec![
            ClosedTask {
                id: "T-B".into(),
                title: "second item".into(),
                closed_unix: 300,
            },
            ClosedTask {
                id: "T-A".into(),
                title: "first item".into(),
                closed_unix: 250,
            },
        ];
        let md = changelog_from_tasks(&closed, 100);
        assert!(md.contains("- T-A: first item\n- T-B: second item\n"));
    }
}
