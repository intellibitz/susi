//! Warn when a commit touches another agent's lane without citing their task.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub agent: String,
    pub globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneWarning {
    pub agent: String,
    pub path: String,
}

/// Simple glob: `*` matches within one path segment; `**` prefix matches any depth suffix.
#[must_use]
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern == "**" || pattern == "*" {
        return true;
    }
    if let Some(suf) = pattern.strip_prefix("**/") {
        return path.ends_with(suf) || path.contains(&format!("/{suf}")) || path == suf;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return path.starts_with(prefix)
            && path[prefix.len()..].starts_with('/')
            && !path[prefix.len() + 1..].contains('/');
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    path == pattern
}

#[must_use]
pub fn lane_overlap_warnings(
    lanes: &[Lane],
    changed_paths: &[&str],
    referenced_agents: &[&str],
) -> Vec<LaneWarning> {
    let mut out = Vec::new();
    for path in changed_paths {
        for lane in lanes {
            if referenced_agents.iter().any(|a| *a == lane.agent) {
                continue;
            }
            if lane.globs.iter().any(|g| glob_matches(g, path)) {
                out.push(LaneWarning {
                    agent: lane.agent.clone(),
                    path: (*path).to_string(),
                });
            }
        }
    }
    out
}

/// Parse minimal lane files: `{ "agent": "...", "globs": ["..."] }`.
pub fn parse_lane_json(s: &str) -> Result<Lane, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
}

#[must_use]
pub fn index_lanes(lanes: Vec<Lane>) -> BTreeMap<String, Lane> {
    lanes.into_iter().map(|l| (l.agent.clone(), l)).collect()
}

#[cfg(test)]
mod lane_overlap_warning_tests {
    use super::*;

    #[test]
    fn lane_overlap_warning_fires_without_cited_lane_task() {
        let lanes = vec![Lane {
            agent: "CLAUDE".into(),
            globs: vec!["crates/susi-gawd/**".into()],
        }];
        let warns = lane_overlap_warnings(&lanes, &["crates/susi-gawd/src/lib.rs"], &[]);
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].agent, "CLAUDE");
        let ok = lane_overlap_warnings(&lanes, &["crates/susi-gawd/src/lib.rs"], &["CLAUDE"]);
        assert!(ok.is_empty());
    }

    #[test]
    fn lane_overlap_warning_ignores_unrelated_paths() {
        let lanes = vec![Lane {
            agent: "DEVIN".into(),
            globs: vec!["scripts/**".into()],
        }];
        assert!(lane_overlap_warnings(&lanes, &["README.md"], &[]).is_empty());
    }

    #[test]
    fn parse_lane_json_roundtrips_and_rejects_malformed_json() {
        let parsed = parse_lane_json(r#"{"agent":"GEMINI","globs":["crates/susi-gawd/*"]}"#)
            .expect("valid lane json");
        assert_eq!(parsed.agent, "GEMINI");
        assert_eq!(parsed.globs, vec!["crates/susi-gawd/*".to_string()]);
        assert!(parse_lane_json("{not valid json").is_err());
    }

    #[test]
    fn index_lanes_maps_by_agent_and_glob_matches_wildcards() {
        assert!(glob_matches("**", "any/nested/path.rs"));
        assert!(glob_matches("**/Cargo.toml", "crates/susi-gawd/Cargo.toml"));
        assert!(glob_matches("crates/*", "crates/lib.rs"));
        assert!(!glob_matches("crates/*", "crates/sub/lib.rs"));

        let indexed = index_lanes(vec![
            Lane {
                agent: "CODEX".into(),
                globs: vec!["**/Cargo.toml".into()],
            },
            Lane {
                agent: "CLAUDE".into(),
                globs: vec!["crates/*".into()],
            },
        ]);
        assert_eq!(indexed.len(), 2);
        assert_eq!(indexed["CODEX"].globs, vec!["**/Cargo.toml".to_string()]);
    }
}
