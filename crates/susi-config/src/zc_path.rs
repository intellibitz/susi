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
        let granted = v.map(str::trim).is_some_and(|trimmed| {
            trimmed == "1"
                || trimmed.eq_ignore_ascii_case("true")
                || trimmed.eq_ignore_ascii_case("yes")
        });
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

    /// Plan a PowerShell `$env:PATH` snippet when consent is granted (`D-OS-7`).
    #[must_use]
    pub fn powershell_path_snippet(&self, bin_dir: &str) -> Option<String> {
        if !self.granted {
            return None;
        }
        Some(format!("$env:PATH = \"{bin_dir};$env:PATH\"  # susi"))
    }

    /// Plan the OS-appropriate PATH snippet (`D-OS-7`).
    #[must_use]
    pub fn path_snippet_for_os(&self, bin_dir: &str, windows: bool) -> Option<String> {
        if windows {
            self.powershell_path_snippet(bin_dir)
        } else {
            self.path_snippet(bin_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_value_accepts_case_insensitive_consent_tokens() {
        for truthy in ["1", "true", "TRUE", "True", "yes", "YES", "  Yes \n"] {
            let consent = PathConsent::from_env_value(Some(truthy));
            assert!(consent.granted, "expected {truthy:?} to grant PATH consent");
        }
        for falsy in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("no"),
            Some("2"),
        ] {
            let consent = PathConsent::from_env_value(falsy);
            assert!(!consent.granted, "expected {falsy:?} to deny PATH consent");
        }
    }

    #[test]
    fn path_snippets_render_posix_and_powershell_for_granted_consent_only() {
        let granted = PathConsent::from_env_value(Some("true"));
        assert_eq!(
            granted.path_snippet("/opt/susi/bin"),
            Some("export PATH=\"/opt/susi/bin:$PATH\"  # susi".into())
        );
        assert_eq!(
            granted.powershell_path_snippet("C:\\susi\\bin"),
            Some("$env:PATH = \"C:\\susi\\bin;$env:PATH\"  # susi".into())
        );
        assert_eq!(
            granted.path_snippet_for_os("/opt/susi/bin", false),
            granted.path_snippet("/opt/susi/bin")
        );
        assert_eq!(
            granted.path_snippet_for_os("C:\\susi\\bin", true),
            granted.powershell_path_snippet("C:\\susi\\bin")
        );

        let denied = PathConsent::from_env_value(None);
        assert_eq!(denied.path_snippet("/opt/susi/bin"), None);
        assert_eq!(denied.powershell_path_snippet("C:\\susi\\bin"), None);
        assert_eq!(denied.path_snippet_for_os("/opt/susi/bin", false), None);
        assert_eq!(denied.path_snippet_for_os("C:\\susi\\bin", true), None);
    }
}
