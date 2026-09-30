//! Detect missing Rust toolchain components and propose rustup fixes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainGap {
    pub component: String,
    pub rustup_cmd: String,
}

/// Report gaps and the exact rustup command to install them.
#[must_use]
pub fn missing_components(present: &[&str], required: &[&str]) -> Vec<ToolchainGap> {
    required
        .iter()
        .filter(|c| !present.iter().any(|p| p == *c))
        .map(|c| ToolchainGap {
            component: (*c).to_string(),
            rustup_cmd: format!("rustup component add {c}"),
        })
        .collect()
}

#[cfg(test)]
mod zc_toolchain_auto_tests {
    use super::*;

    #[test]
    fn zc_toolchain_auto_offers_rustup_for_gaps() {
        let gaps = missing_components(&["rustc", "cargo"], &["rustfmt", "clippy", "cargo"]);
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].rustup_cmd, "rustup component add rustfmt");
        assert!(gaps.iter().any(|g| g.component == "clippy"));
    }
}
