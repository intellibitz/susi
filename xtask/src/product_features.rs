//! The Cargo feature selection of the shipped Susi product.
//!
//! The default `susi` graph is the slim protocol socket (`default = []`): it
//! leaves the Candle local model engine out (T-GROKSLIM1104-1). A qualified
//! release must answer its first goals with local inference (Step 1 acceptance:
//! two audited nonempty Candle generations), so every release step (local
//! builds, the production build gate, the dependency inventory and the advisory
//! scan) selects the same product features: no defaults, plus `local-engine`.
//! The inventory and scans therefore cover exactly the graph that ships.
//!
//! Minimal release-machinery fixtures that do not declare `local-engine` keep
//! the bare `--no-default-features` selection, so their gates still run.

use std::path::Path;

/// The product feature the shipped binary needs beyond the slim default.
pub const PRODUCT_ENGINE_FEATURE: &str = "local-engine";

/// Whether the workspace manifest at `root` declares the product engine feature
/// in its `[features]` table.
pub fn declares_engine_feature(root: &Path) -> bool {
    let Ok(manifest) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return false;
    };
    let mut in_features = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_features = line == "[features]";
            continue;
        }
        if in_features
            && line
                .strip_prefix(PRODUCT_ENGINE_FEATURE)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        {
            return true;
        }
    }
    false
}

/// The Cargo feature arguments of the shipped product built from `root`.
pub fn product_feature_args(root: &Path) -> Vec<String> {
    let mut args = vec!["--no-default-features".to_string()];
    if declares_engine_feature(root) {
        args.push("--features".to_string());
        args.push(PRODUCT_ENGINE_FEATURE.to_string());
    }
    args
}

fn select_feature_args(args: &mut Vec<String>, product: &[String]) {
    let mut selected = Vec::with_capacity(args.len() + product.len());
    for arg in args.drain(..) {
        if arg == "--no-default-features" {
            selected.extend(product.iter().cloned());
        } else {
            selected.push(arg);
        }
    }
    *args = selected;
}

/// Replaces the bare `--no-default-features` selection in `args` with the
/// product's feature arguments for `root`.
pub fn select_product_features(args: &mut Vec<String>, root: &Path) {
    let product = product_feature_args(root);
    select_feature_args(args, &product);
}

/// Replaces the bare feature selection with the immutable production policy.
///
/// Complete gate receipts are validated after the temporary candidate checkout
/// has been removed.  This path therefore must not rediscover product features
/// from `Cargo.toml`; the shipped product policy is already fixed by this
/// release machinery source and is recorded in the gate argv.
pub fn select_qualified_product_features(args: &mut Vec<String>) {
    let product = [
        "--no-default-features".to_string(),
        "--features".to_string(),
        PRODUCT_ENGINE_FEATURE.to_string(),
    ];
    select_feature_args(args, &product);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str, manifest: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-product-features-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = manifest {
            std::fs::write(dir.join("Cargo.toml"), text).unwrap();
        }
        dir
    }

    #[test]
    fn the_product_selects_the_engine_when_the_workspace_declares_it() {
        let dir = scratch(
            "declared",
            Some("[package]\nname = \"susi\"\n\n[features]\ndefault = []\nlocal-engine = [\"a/b\"]\nplugins = [\"local-engine\"]\n"),
        );
        assert_eq!(
            product_feature_args(&dir),
            ["--no-default-features", "--features", "local-engine"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fixtures_without_the_engine_feature_keep_the_bare_selection() {
        for (tag, manifest) in [
            ("missing", None),
            (
                "absent",
                Some("[package]\nname = \"fixture\"\n[features]\ndefault = []\n"),
            ),
            // Mentioning the name outside [features] is not a declaration.
            (
                "elsewhere",
                Some("[package]\nname = \"fixture\"\n[dependencies]\nlocal-engine = \"1\"\n"),
            ),
            ("prefix", Some("[features]\nlocal-engine-extra = []\n")),
        ] {
            let dir = scratch(tag, manifest);
            assert_eq!(
                product_feature_args(&dir),
                ["--no-default-features"],
                "{tag}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn selection_replaces_only_the_bare_feature_flag() {
        let dir = scratch(
            "select",
            Some("[features]\ndefault = []\nlocal-engine = []\n"),
        );
        let mut args: Vec<String> = [
            "build",
            "--release",
            "--no-default-features",
            "--target",
            "x",
        ]
        .map(String::from)
        .to_vec();
        select_product_features(&mut args, &dir);
        assert_eq!(
            args,
            [
                "build",
                "--release",
                "--no-default-features",
                "--features",
                "local-engine",
                "--target",
                "x"
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn qualified_selection_does_not_need_the_candidate_manifest() {
        let mut args: Vec<String> = [
            "build",
            "--release",
            "--no-default-features",
            "--target",
            "x",
        ]
        .map(String::from)
        .to_vec();
        select_qualified_product_features(&mut args);
        assert_eq!(
            args,
            [
                "build",
                "--release",
                "--no-default-features",
                "--features",
                "local-engine",
                "--target",
                "x"
            ]
        );
    }

    #[test]
    fn this_workspace_ships_with_the_local_engine() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(declares_engine_feature(&root));
    }
}
