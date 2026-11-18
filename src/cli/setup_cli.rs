//! `susi setup`: one end-to-end zero-config ecosystem setup command
//! (VC-201-069). Since 2026-11-17 (Mandate 84) Susi Commercial Apps serves it
//! (`susi-commercial setup`): this forwards the same subcommand and flags, in
//! this working directory.

pub use susi_cli_defs::setup::*;

/// The `susi-commercial` words for `action`.
fn words(action: Option<SetupCommands>) -> Vec<String> {
    let mut words = vec!["setup".to_string()];
    let flag = |words: &mut Vec<String>, on: bool, name: &str| {
        if on {
            words.push(name.to_string());
        }
    };
    match action {
        None => {}
        Some(SetupCommands::Plan { json }) => {
            words.push("plan".into());
            flag(&mut words, json, "--json");
        }
        Some(SetupCommands::Apply { cloud, json }) => {
            words.push("apply".into());
            flag(&mut words, cloud, "--cloud");
            flag(&mut words, json, "--json");
        }
    }
    words
}

/// Execute `susi setup` by forwarding to `susi-commercial setup`.
pub fn execute(action: Option<SetupCommands>) -> i32 {
    super::peers_cli::forward("setup", &words(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_subcommands_parse_plan_and_apply_flags() {
        use crate::cli::defs::Cli;
        use clap::Parser;

        Cli::try_parse_from(["susi", "setup", "plan", "--json"]).expect("parse setup plan --json");
        Cli::try_parse_from(["susi", "setup", "apply", "--cloud", "--json"])
            .expect("parse setup apply --cloud --json");
    }

    #[test]
    fn every_subcommand_and_flag_is_forwarded() {
        assert_eq!(words(None), ["setup"]);
        assert_eq!(
            words(Some(SetupCommands::Plan { json: true })),
            ["setup", "plan", "--json"]
        );
        assert_eq!(
            words(Some(SetupCommands::Apply {
                cloud: true,
                json: false
            })),
            ["setup", "apply", "--cloud"]
        );
    }

    /// The first-run inventory `susi status` reports, read through the
    /// daemon's host views (formerly reached through `susi setup`).
    #[test]
    fn opening_model_inventory_includes_reused_paths_without_recursing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "susi-opening-inventory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let shared = root.join("shared");
        std::fs::create_dir_all(root.join("models"))?;
        std::fs::create_dir_all(shared.join("nested"))?;
        std::fs::write(root.join("models/own.gguf"), b"fixture")?;
        std::fs::write(shared.join("reused.GGUF"), b"fixture")?;
        std::fs::write(shared.join("nested/not-scanned.gguf"), b"fixture")?;
        std::fs::write(shared.join("ignore.json"), b"{}")?;
        std::fs::write(
            root.join("config.json"),
            serde_json::to_vec(
                &serde_json::json!({"local_scan_paths": [shared, shared, root.join("absent")]}),
            )?,
        )?;
        assert_eq!(
            susi_daemon::host_views::opening_model_inventory(&root, &root),
            vec!["own.gguf", "reused.GGUF"]
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opening_model_inventory_returns_empty_for_missing_directories() {
        let missing =
            std::env::temp_dir().join(format!("susi-opening-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(susi_daemon::host_views::opening_model_inventory(&missing, &missing).is_empty());
    }
}
