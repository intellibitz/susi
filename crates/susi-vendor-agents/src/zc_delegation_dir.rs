//! Delegation ingress directory default.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationDir {
    pub path: PathBuf,
}

impl DelegationDir {
    #[must_use]
    pub fn default_under(home: &Path) -> Self {
        Self {
            path: home.join("delegations"),
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.path)
    }
}

#[cfg(test)]
mod zc_delegation_dir_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn zc_delegation_dir_defaults_and_creates() {
        let home = std::env::temp_dir().join(format!(
            "susi-del-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let d = DelegationDir::default_under(&home);
        d.ensure().unwrap();
        assert!(d.path.is_dir());
        let _ = std::fs::remove_dir_all(&home);
    }
}
