//! Uninstall plan: everything the installer created.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallPlan {
    pub paths: Vec<String>,
}

impl UninstallPlan {
    #[must_use]
    pub fn default_for_home(home: &str) -> Self {
        Self {
            paths: vec![
                format!("{home}/.susi/bin/susi"),
                format!("{home}/.susi/config"),
                format!("{home}/.susi"),
            ],
        }
    }

    #[must_use]
    pub fn uninstall_paths(&self) -> &[String] {
        &self.paths
    }
}
