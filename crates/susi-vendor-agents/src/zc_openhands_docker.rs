//! OpenHands docker enablement without a compose file edit.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenHandsDocker {
    pub image: String,
    pub enabled: bool,
}

impl Default for OpenHandsDocker {
    fn default() -> Self {
        Self {
            image: "ghcr.io/all-hands-ai/openhands:latest".into(),
            enabled: false,
        }
    }
}

impl OpenHandsDocker {
    pub fn enable_default(&mut self) {
        self.enabled = true;
    }
}

#[cfg(test)]
mod zc_openhands_docker_tests {
    use super::*;

    #[test]
    fn zc_openhands_docker_enables_default_image() {
        let mut d = OpenHandsDocker::default();
        assert!(!d.enabled);
        d.enable_default();
        assert!(d.enabled);
        assert!(d.image.contains("openhands"));
    }
}
