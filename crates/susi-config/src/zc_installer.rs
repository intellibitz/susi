//! Default installer flags: auto-update and daemon on.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallFlags {
    pub auto_update: bool,
    pub daemon: bool,
}

impl Default for InstallFlags {
    fn default() -> Self {
        Self {
            auto_update: true,
            daemon: true,
        }
    }
}

#[must_use]
pub fn default_install_flags() -> InstallFlags {
    InstallFlags::default()
}
