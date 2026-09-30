//! Use existing gcloud/az/aws logins for Vertex, Azure and Bedrock.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudCliAuth {
    pub provider: String,
    pub usable: bool,
    pub source: String,
}

/// Map detected CLI login state to cloud provider auth.
#[must_use]
pub fn from_cli(gcloud: bool, az: bool, aws: bool) -> Vec<CloudCliAuth> {
    vec![
        CloudCliAuth {
            provider: "vertex".into(),
            usable: gcloud,
            source: "gcloud auth".into(),
        },
        CloudCliAuth {
            provider: "azure_openai".into(),
            usable: az,
            source: "az login".into(),
        },
        CloudCliAuth {
            provider: "bedrock".into(),
            usable: aws,
            source: "aws credentials".into(),
        },
    ]
}

#[cfg(test)]
mod zc_cloud_cli_auth_tests {
    use super::*;

    #[test]
    fn zc_cloud_cli_auth_maps_existing_logins() {
        let a = from_cli(true, false, true);
        assert!(a.iter().any(|x| x.provider == "vertex" && x.usable));
        assert!(a.iter().any(|x| x.provider == "azure_openai" && !x.usable));
        assert!(a.iter().any(|x| x.provider == "bedrock" && x.usable));
    }
}
