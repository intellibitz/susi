//! Discover Devin org id from API/env without a hand-edited file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevinOrg {
    pub org_id: String,
}

#[must_use]
pub fn discover_devin_org(env_org: Option<&str>, api_json: Option<&str>) -> Option<DevinOrg> {
    if let Some(id) = env_org.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(DevinOrg {
            org_id: id.to_string(),
        });
    }
    let raw = api_json?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let id = v
        .get("organization_id")
        .or_else(|| v.get("org_id"))
        .and_then(|x| x.as_str())?;
    Some(DevinOrg {
        org_id: id.to_string(),
    })
}

#[cfg(test)]
mod zc_devin_org_tests {
    use super::*;

    #[test]
    fn zc_devin_org_from_api_or_env() {
        assert_eq!(
            discover_devin_org(Some("env-org"), None).unwrap().org_id,
            "env-org"
        );
        assert_eq!(
            discover_devin_org(None, Some(r#"{"organization_id":"api-org"}"#))
                .unwrap()
                .org_id,
            "api-org"
        );
        assert!(discover_devin_org(None, None).is_none());
    }
}
