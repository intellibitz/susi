//! One-consent PATH install for the susi binary (zero-config).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathConsent {
    pub granted: bool,
}

impl PathConsent {
    pub const ENV: &'static str = "SUSI_PATH_CONSENT";

    #[must_use]
    pub fn from_env_value(v: Option<&str>) -> Self {
        let granted = matches!(v.map(str::trim), Some("1") | Some("true") | Some("yes"));
        Self { granted }
    }

    /// Plan a PATH snippet only when consent is granted.
    #[must_use]
    pub fn path_snippet(&self, bin_dir: &str) -> Option<String> {
        if !self.granted {
            return None;
        }
        Some(format!("export PATH=\"{bin_dir}:$PATH\"  # susi"))
    }
}
