//! Plan validation against the repository.
//!
//! Structural validation (unique keys, non-empty fields) is not enough: a
//! model authored `cargo test -p gmcp` when the package is `susi-gmcp`, and
//! the mistake would only surface an hour into the agent run. This module
//! checks the plan against the workspace before anything is dispatched:
//! package names resolve, scopes live under a directory the repo already has,
//! and an acceptance is test-shaped.

use crate::supervisor::Plan;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use susi_error::{EaiError, EaiResult};

/// What the repository actually contains.
pub struct RepoFacts {
    /// Repository root.
    pub root: PathBuf,
    /// Workspace package names.
    pub packages: BTreeSet<String>,
}

impl RepoFacts {
    /// Read the workspace's package names with `cargo metadata --no-deps`.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::process` when cargo cannot run or its output cannot
    /// be parsed.
    pub fn load(root: &Path) -> EaiResult<Self> {
        let output = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(root)
            .output()
            .map_err(|error| EaiError::process(format!("cargo metadata: {error}")))?;
        if !output.status.success() {
            return Err(EaiError::process(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| EaiError::process(format!("cargo metadata json: {error}")))?;
        let packages = value
            .get("packages")
            .and_then(Value::as_array)
            .map(|packages| {
                packages
                    .iter()
                    .filter_map(|package| package.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            root: root.to_path_buf(),
            packages,
        })
    }
}

/// Validate every task's acceptance and scopes against the repository.
///
/// # Errors
///
/// Returns `EaiError::config` naming the task and the first violation.
pub fn validate_plan(plan: &Plan, facts: &RepoFacts) -> EaiResult<()> {
    plan.validate()?;
    for task in &plan.tasks {
        validate_acceptance(&task.accept, &task.key, facts)?;
        for scope in &task.scopes {
            validate_scope(scope, &task.key, facts)?;
        }
    }
    Ok(())
}

fn validate_acceptance(accept: &str, key: &str, facts: &RepoFacts) -> EaiResult<()> {
    let argv: Vec<&str> = accept.split_whitespace().collect();
    let Some(first) = argv.first().copied() else {
        return Err(EaiError::config(format!("task {key}: empty acceptance")));
    };
    match first {
        "cargo" => {
            let subcommand = argv.get(1).copied().unwrap_or_default();
            if subcommand != "test" && subcommand != "nextest" {
                return Err(EaiError::config(format!(
                    "task {key}: acceptance must be `cargo test` or `cargo nextest`"
                )));
            }
            let mut packages = Vec::new();
            for window in argv.windows(2) {
                if window[0] == "-p" || window[0] == "--package" {
                    packages.push(window[1]);
                }
            }
            if packages.is_empty() {
                return Err(EaiError::config(format!(
                    "task {key}: acceptance names no package"
                )));
            }
            for package in packages {
                if !facts.packages.contains(package) {
                    return Err(EaiError::config(format!(
                        "task {key}: package `{package}` is not in this workspace"
                    )));
                }
            }
            Ok(())
        }
        _ if first.starts_with("scripts/") => {
            let script = facts.root.join(first);
            if script.is_file() {
                Ok(())
            } else {
                Err(EaiError::config(format!(
                    "task {key}: `{first}` is not an existing checker"
                )))
            }
        }
        _ => Err(EaiError::config(format!(
            "task {key}: acceptance must be `cargo test|nextest ... -p <crate>` or an existing `scripts/` checker"
        ))),
    }
}

fn validate_scope(scope: &str, key: &str, facts: &RepoFacts) -> EaiResult<()> {
    let path = Path::new(scope);
    if scope.trim().is_empty() {
        return Err(EaiError::config(format!("task {key}: empty scope")));
    }
    if path.is_absolute() {
        return Err(EaiError::config(format!(
            "task {key}: scope `{scope}` must be repo-relative"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(EaiError::config(format!(
            "task {key}: scope `{scope}` may not escape the repository"
        )));
    }
    let Some(first) = path.components().next() else {
        return Err(EaiError::config(format!("task {key}: empty scope")));
    };
    let anchor = facts.root.join(first);
    if anchor.exists() {
        Ok(())
    } else {
        Err(EaiError::config(format!(
            "task {key}: scope `{scope}` is not under an existing directory"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::PlanTask;

    fn facts(dir: &Path) -> RepoFacts {
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
        RepoFacts {
            root: dir.to_path_buf(),
            packages: ["susi-harness".to_string()].into_iter().collect(),
        }
    }

    fn plan(accept: &str, scope: &str) -> Plan {
        Plan {
            mission: "m".into(),
            tasks: vec![PlanTask {
                key: "a".into(),
                title: "t".into(),
                accept: accept.into(),
                scopes: vec![scope.into()],
                model: None,
                prompt: "p".into(),
            }],
        }
    }

    #[test]
    fn a_resolvable_plan_passes() {
        let dir = tempfile::tempdir().unwrap();
        let facts = facts(dir.path());
        assert!(validate_plan(
            &plan("cargo test -p susi-harness", "docs/harness.md"),
            &facts
        )
        .is_ok());
        assert!(validate_plan(
            &plan("cargo nextest run -p susi-harness -E test(x)", "docs"),
            &facts
        )
        .is_ok());
        assert!(validate_plan(&plan("scripts/check.sh", "docs"), &facts).is_ok());
    }

    #[test]
    fn an_unknown_package_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let facts = facts(dir.path());
        let error = validate_plan(&plan("cargo test -p gmcp", "docs"), &facts).unwrap_err();
        assert!(error.to_string().contains("not in this workspace"));
    }

    #[test]
    fn a_non_test_acceptance_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let facts = facts(dir.path());
        assert!(validate_plan(&plan("cargo check -p susi-harness", "docs"), &facts).is_err());
        assert!(validate_plan(&plan("scripts/missing.sh", "docs"), &facts).is_err());
        assert!(validate_plan(&plan("cargo --version", "docs"), &facts).is_err());
    }

    #[test]
    fn a_scope_outside_the_repository_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let facts = facts(dir.path());
        assert!(
            validate_plan(&plan("cargo test -p susi-harness", "nope/file.md"), &facts).is_err()
        );
        assert!(validate_plan(&plan("cargo test -p susi-harness", "../escape"), &facts).is_err());
    }
}
