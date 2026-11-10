#![forbid(unsafe_code)]

//! Deterministic functional vs. non-functional commit and task classification
//! for release distillation (`--functional-only`).
//!
//! Development `main` accumulates non-functional housekeeping commits alongside
//! verified product work:
//! - gated landing receipts (`.agents/receipts/**`)
//! - task/evidence/governance records (`.agents/tasks/**`, `.agents/evidence/**`,
//!   `.agents/{identity,roadmap,evidence}.json`)
//! - release-recording / version-sync commits (`releases/**`, `release: record ...`,
//!   `chore(release): ...`)
//! - pure documentation updates (`docs/**`, `*.md`)
//!
//! Under `--functional-only`, release distillation includes only tasks backed by
//! at least one functional commit and strips non-functional housekeeping commits
//! (such as post-landing `.agents/receipts/*` commits carrying a `Task:` trailer)
//! from attributed task commit lists.

/// Classification of a repository commit or task diff for functional distillation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitClassification {
    FunctionalCore,
    NonFunctional,
}

impl CommitClassification {
    #[must_use]
    pub const fn is_functional(self) -> bool {
        match self {
            Self::FunctionalCore => true,
            Self::NonFunctional => false,
        }
    }
}

/// Normalizes a repository-relative path for deterministic classification.
#[must_use]
pub fn normalize_repo_path(raw: &str) -> String {
    let unified = raw.trim().replace('\\', "/");
    let mut slice = unified.as_str();
    while let Some(rest) = slice.strip_prefix("./") {
        slice = rest;
    }
    slice = slice.trim_start_matches('/').trim_end_matches('/');
    slice.to_owned()
}

/// Returns `true` when `raw_path` touches functional workspace content
/// (`crates/**`, `src/**`, `xtask/**`, `tests/**`, `Cargo.toml`, `Cargo.lock`,
/// `build.rs`, `deny.toml`, `config/**`, `LICENSES/**`, `THIRD_PARTY_NOTICES`,
/// `vendor/**`, `.agents/harvest/**`, etc.) rather than housekeeping records or
/// documentation.
#[must_use]
pub fn is_functional_path(raw_path: &str) -> bool {
    let path = normalize_repo_path(raw_path);
    if path.is_empty() {
        return false;
    }
    if path == "releases" || path.starts_with("releases/") {
        return false;
    }
    if path == "docs" || path.starts_with("docs/") {
        return false;
    }
    if path.len() >= 3 && path[path.len() - 3..].eq_ignore_ascii_case(".md") {
        return false;
    }
    if path == ".agents" {
        return false;
    }
    if let Some(rest) = path.strip_prefix(".agents/") {
        // External Apache-2.0 code harvest provenance manifests live in
        // `.agents/harvest/*.json` and are required on the distilled tree by
        // Mandate 74/75 harvest verification. All other `.agents/**` paths
        // (receipts, tasks, evidence, schemas, identity, roadmap) are
        // non-functional housekeeping.
        return rest == "harvest" || rest.starts_with("harvest/");
    }
    true
}

fn is_version_sync_manifest_path(norm_path: &str) -> bool {
    if matches!(norm_path, "Cargo.toml" | "Cargo.lock" | "xtask/Cargo.toml") {
        return true;
    }
    if let Some(rest) = norm_path.strip_prefix("crates/") {
        if let Some((crate_dir, tail)) = rest.split_once('/') {
            return !crate_dir.is_empty() && tail == "Cargo.toml";
        }
    }
    false
}

fn has_housekeeping_subject_prefix(subject: &str) -> bool {
    let lower = subject.trim().to_ascii_lowercase();
    lower.starts_with("release: record ")
        || lower.starts_with("chore(release):")
        || lower.starts_with("chore(tasks):")
        || lower.starts_with("receipt:")
}

/// Returns `true` when a commit is a main-branch release-recording or
/// version-advance housekeeping commit (for example
/// `release: record v0.21.63 and advance main to 0.21.63` or
/// `chore(release): record ...`) whose only functional-looking paths are Cargo
/// version manifests/lockfiles.
#[must_use]
pub fn is_release_record_housekeeping(subject: &str, changed_paths: &[String]) -> bool {
    let touches_releases_dir = changed_paths.iter().any(|p| {
        let norm = normalize_repo_path(p);
        norm == "releases" || norm.starts_with("releases/")
    });
    if !touches_releases_dir && !has_housekeeping_subject_prefix(subject) {
        return false;
    }
    changed_paths.iter().all(|p| {
        let norm = normalize_repo_path(p);
        !is_functional_path(&norm) || is_version_sync_manifest_path(&norm)
    })
}

/// Classifies a single commit from its subject line and changed file paths.
#[must_use]
pub fn classify_commit(subject: &str, changed_paths: &[String]) -> CommitClassification {
    if is_release_record_housekeeping(subject, changed_paths) {
        return CommitClassification::NonFunctional;
    }
    if changed_paths.iter().any(|p| is_functional_path(p)) {
        CommitClassification::FunctionalCore
    } else {
        CommitClassification::NonFunctional
    }
}

/// Convenience predicate returning `true` iff `classify_commit` is
/// [`CommitClassification::FunctionalCore`].
#[must_use]
pub fn is_functional_commit(subject: &str, changed_paths: &[String]) -> bool {
    classify_commit(subject, changed_paths).is_functional()
}

/// Extracts a canonical uppercase `T-...` task identifier when `raw_path` names
/// a task manifest under `.agents/tasks/{,done/,in-progress/}<ID>.json`.
#[must_use]
pub fn task_id_from_manifest_path(raw_path: &str) -> Option<String> {
    let path = normalize_repo_path(raw_path);
    let rest = path.strip_prefix(".agents/tasks/")?;
    let file_name = if let Some(done_rest) = rest.strip_prefix("done/") {
        done_rest
    } else if let Some(in_prog_rest) = rest.strip_prefix("in-progress/") {
        in_prog_rest
    } else {
        rest
    };
    if file_name.contains('/') {
        return None;
    }
    let stem = file_name.strip_suffix(".json")?.trim();
    if stem.len() > 2 && (stem.starts_with("T-") || stem.starts_with("t-")) {
        Some(stem.to_ascii_uppercase())
    } else {
        None
    }
}

/// Returns `true` when `task_id` names a cross-repository Commercial Apps task
/// (`T-*COMM*`, such as `T-CLAUDECOMMCHAT-1`) recorded on the shared multi-repo
/// coordination board rather than a Core task manifest.
#[must_use]
pub fn is_cross_repo_commercial_task_id(task_id: &str) -> bool {
    let upper = task_id.trim().to_ascii_uppercase();
    upper.starts_with("T-") && upper.contains("COMM")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn path_classification_separates_functional_code_from_housekeeping() {
        for functional in [
            "crates/susi-core/src/lib.rs",
            "src/main.rs",
            "xtask/src/distill.rs",
            "tests/functional_distillation.rs",
            "Cargo.toml",
            "Cargo.lock",
            "build.rs",
            "deny.toml",
            "config/default.toml",
            "LICENSES/Apache-2.0.txt",
            "THIRD_PARTY_NOTICES",
            "vendor/minijinja/src/lib.rs",
            ".agents/harvest/H-TEST-1.json",
        ] {
            assert!(
                is_functional_path(functional),
                "expected functional path: {functional}"
            );
        }

        for non_functional in [
            "",
            ".agents/receipts/e54a332c8713.json",
            ".agents/evidence/gate.json",
            ".agents/tasks/T-FOO-1.json",
            ".agents/tasks/done/T-FOO-1.json",
            ".agents/identity.json",
            ".agents/roadmap.json",
            "releases/v0.21.63.json",
            "releases/qualification/v0.21.63.json",
            "releases/distill/v0-21-63-rc1.json",
            "docs/lean-core.md",
            "docs/architecture/overview.txt",
            "README.md",
            "AGENTS.md",
            "CLAUDE.md",
            "GEMINI.md",
            "VISION.md",
        ] {
            assert!(
                !is_functional_path(non_functional),
                "expected non-functional path: {non_functional}"
            );
        }
    }

    #[test]
    fn commit_classification_handles_receipts_docs_release_records_and_real_deps() {
        assert_eq!(
            classify_commit(
                "feat(core): add multi-turn session replay",
                &paths(&[
                    "crates/susi-core/src/acp.rs",
                    "docs/lean-core.md",
                    ".agents/tasks/T-CTOCORELEAN-1.json",
                ])
            ),
            CommitClassification::FunctionalCore
        );

        assert_eq!(
            classify_commit(
                "receipt: gated landing e54a332c8713d64fd50bc75f1a94beafed16cb53",
                &paths(&[".agents/receipts/e54a332c8713d64fd50bc75f1a94beafed16cb53.json"])
            ),
            CommitClassification::NonFunctional
        );

        assert_eq!(
            classify_commit(
                "docs(governance): codify Mandate 75",
                &paths(&[
                    "AGENTS.md",
                    "README.md",
                    "docs/distillation.md",
                    ".agents/identity.json",
                    ".agents/tasks/T-DOC-1.json",
                ])
            ),
            CommitClassification::NonFunctional
        );

        assert_eq!(
            classify_commit(
                "release: record v0.21.63 and advance main to 0.21.63",
                &paths(&[
                    "Cargo.toml",
                    "Cargo.lock",
                    "crates/susi-core/Cargo.toml",
                    "README.md",
                    "releases/v0.21.63.json",
                    "releases/qualification/v0.21.63.json",
                    ".agents/tasks/T-CTOREL63-2.json",
                ])
            ),
            CommitClassification::NonFunctional
        );

        assert_eq!(
            classify_commit(
                "chore(release): record v0.1.0",
                &paths(&[
                    "Cargo.toml",
                    "releases/v0.1.0.json",
                    ".agents/tasks/T-REL-1.json",
                ])
            ),
            CommitClassification::NonFunctional
        );

        // Genuine dependency change without release-record housekeeping remains functional.
        assert_eq!(
            classify_commit(
                "chore(deps): drop unused crate from workspace",
                &paths(&["Cargo.toml", "Cargo.lock", ".agents/tasks/T-DEPS-1.json"])
            ),
            CommitClassification::FunctionalCore
        );
    }

    #[test]
    fn extracts_task_id_from_task_manifest_paths() {
        assert_eq!(
            task_id_from_manifest_path(".agents/tasks/T-CODEXRELEASELANES-1.json").as_deref(),
            Some("T-CODEXRELEASELANES-1")
        );
        assert_eq!(
            task_id_from_manifest_path(".agents/tasks/done/t-abc-2.json").as_deref(),
            Some("T-ABC-2")
        );
        assert_eq!(
            task_id_from_manifest_path(".agents/tasks/in-progress/T-WIP-9.json").as_deref(),
            Some("T-WIP-9")
        );
        assert_eq!(task_id_from_manifest_path(".agents/tasks/notes.md"), None);
        assert_eq!(
            task_id_from_manifest_path("crates/susi-core/src/lib.rs"),
            None
        );
    }

    #[test]
    fn identifies_cross_repo_commercial_task_ids() {
        assert!(is_cross_repo_commercial_task_id("T-CLAUDECOMMCHAT-1"));
        assert!(is_cross_repo_commercial_task_id("t-ctocommwave3-1"));
        assert!(!is_cross_repo_commercial_task_id("T-CODEXHISTORY-1"));
        assert!(!is_cross_repo_commercial_task_id("T-GHOST-1"));
        assert!(!is_cross_repo_commercial_task_id("COMM-1"));
    }
}
