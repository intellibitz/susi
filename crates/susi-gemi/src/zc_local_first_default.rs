//! Local-first default: a working answer with zero cloud keys.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteChoice {
    pub target: String,
}

#[must_use]
pub fn local_first_default(has_local_model: bool, has_cloud_key: bool) -> RouteChoice {
    if has_local_model {
        RouteChoice {
            target: "local".into(),
        }
    } else if has_cloud_key {
        RouteChoice {
            target: "cloud".into(),
        }
    } else {
        RouteChoice {
            target: "prompt_pull_local".into(),
        }
    }
}

#[cfg(test)]
mod zc_local_first_default_tests {
    use super::*;

    #[test]
    fn zc_local_first_default_prefers_local_with_zero_keys() {
        assert_eq!(local_first_default(true, false).target, "local");
        assert_eq!(
            local_first_default(false, false).target,
            "prompt_pull_local"
        );
        assert_eq!(local_first_default(false, true).target, "cloud");
    }
}
