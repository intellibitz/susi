//! Detect gh auth up front for repo automation scripts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhAuthStatus {
    pub authenticated: bool,
    pub fix_cmd: Option<String>,
}

/// Inspect whether `gh` is usable; if not, return the exact login step.
#[must_use]
pub fn gh_auth_status(gh_present: bool, logged_in: bool) -> GhAuthStatus {
    if !gh_present {
        return GhAuthStatus {
            authenticated: false,
            fix_cmd: Some("install gh, then: gh auth login".into()),
        };
    }
    if logged_in {
        GhAuthStatus {
            authenticated: true,
            fix_cmd: None,
        }
    } else {
        GhAuthStatus {
            authenticated: false,
            fix_cmd: Some("gh auth login".into()),
        }
    }
}

#[cfg(test)]
mod zc_gh_auth_auto_tests {
    use super::*;

    #[test]
    fn zc_gh_auth_auto_points_at_login() {
        let ok = gh_auth_status(true, true);
        assert!(ok.authenticated);
        assert!(ok.fix_cmd.is_none());
        let need = gh_auth_status(true, false);
        assert!(!need.authenticated);
        assert_eq!(need.fix_cmd.as_deref(), Some("gh auth login"));
    }
}
