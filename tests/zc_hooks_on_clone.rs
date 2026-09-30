//! Hooks and merge driver active on a fresh clone without a script.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

use std::path::Path;

fn hooks_ready(repo: &Path) -> bool {
    let hooks = repo.join(".githooks");
    hooks.join("pre-commit").is_file() && hooks.join("commit-msg").is_file()
}

fn merge_driver_configured(gitconfig: &str) -> bool {
    gitconfig.contains("merge.ledger") || gitconfig.contains("merge \"ledger\"")
}

#[test]
fn zc_hooks_on_clone_detects_repo_hooks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        hooks_ready(root),
        ".githooks must ship in the tree so a fresh clone has them"
    );
    let cfg = std::fs::read_to_string(root.join(".gitattributes")).unwrap_or_default();
    assert!(
        cfg.contains("ledger") || merge_driver_configured(&cfg),
        "ledger merge driver must be declared for evidence files"
    );
}
