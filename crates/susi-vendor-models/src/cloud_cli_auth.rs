//! Reuse existing cloud CLI logins for Vertex, Azure OpenAI and Bedrock
//! (T-CLAUDE-183): when `gcloud`, `az` or `aws` is already authenticated we
//! plan provider auth from it instead of demanding a fresh key. Pure
//! planning over a caller-supplied detection snapshot — no CLI is invoked
//! here; tests supply the snapshot.

/// What the host probe found, per CLI.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CliState {
    /// Binary exists on PATH.
    pub installed: bool,
    /// An account/identity is configured (non-interactive calls succeed).
    pub logged_in: bool,
    /// For gcloud: `gcloud auth application-default` credentials exist.
    /// For aws: a credential source resolves (env/config/role). For az:
    /// `az account show` succeeds — same meaning as `logged_in`.
    pub app_default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DetectedClis {
    pub gcloud: CliState,
    pub az: CliState,
    pub aws: CliState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderAuth {
    pub provider: &'static str,
    /// How susi should authenticate.
    pub method: &'static str,
    /// Concrete command/config the runtime uses.
    pub detail: String,
    /// `false` when the CLI exists but login is missing — we then describe
    /// the one command the user runs, not a failure.
    pub ready: bool,
}

/// Map detected CLI state to provider auth plans. Every cloud provider gets
/// a plan entry so the UI can show "ready" vs "one command away".
pub fn auth_plan(d: &DetectedClis) -> Vec<ProviderAuth> {
    vec![
        plan(
            "vertex",
            "gcloud adc",
            d.gcloud.installed && d.gcloud.logged_in && d.gcloud.app_default,
            if d.gcloud.installed && d.gcloud.logged_in && d.gcloud.app_default {
                "use `gcloud auth application-default print-access-token` per request".into()
            } else {
                "run `gcloud auth application-default login` once — no key file needed".into()
            },
        ),
        plan(
            "azure-openai",
            "az token",
            d.az.installed && d.az.logged_in,
            if d.az.installed && d.az.logged_in {
                "use `az account get-access-token --scope https://cognitiveservices.azure.com/.default`".into()
            } else {
                "run `az login` once — no API key needed".into()
            },
        ),
        plan(
            "bedrock",
            "aws sigv4",
            d.aws.installed && d.aws.logged_in,
            if d.aws.installed && d.aws.logged_in {
                "sign requests with the ambient AWS credential chain (env/config/role)".into()
            } else {
                "run `aws configure sso` or `aws configure` once — no key entry needed".into()
            },
        ),
    ]
}

fn plan(provider: &'static str, method: &'static str, ready: bool, detail: String) -> ProviderAuth {
    ProviderAuth {
        provider,
        method,
        ready,
        detail,
    }
}

/// Providers that can serve right now without any stored key.
pub fn ready_providers(d: &DetectedClis) -> Vec<&'static str> {
    auth_plan(d)
        .into_iter()
        .filter(|p| p.ready)
        .map(|p| p.provider)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zc_cloud_cli_auth_gcloud_adc_enables_vertex() {
        let d = DetectedClis {
            gcloud: CliState {
                installed: true,
                logged_in: true,
                app_default: true,
            },
            ..DetectedClis::default()
        };
        assert_eq!(ready_providers(&d), ["vertex"]);
    }

    #[test]
    fn zc_cloud_cli_auth_az_login_enables_azure() {
        let d = DetectedClis {
            az: CliState {
                installed: true,
                logged_in: true,
                app_default: false,
            },
            ..DetectedClis::default()
        };
        let plan = auth_plan(&d);
        let az = plan.iter().find(|p| p.provider == "azure-openai").unwrap();
        assert!(az.ready);
        assert!(az.detail.contains("az account get-access-token"));
    }

    #[test]
    fn zc_cloud_cli_auth_aws_chain_enables_bedrock() {
        let d = DetectedClis {
            aws: CliState {
                installed: true,
                logged_in: true,
                app_default: true,
            },
            ..DetectedClis::default()
        };
        assert!(ready_providers(&d).contains(&"bedrock"));
    }

    #[test]
    fn zc_cloud_cli_auth_logged_out_gives_one_command_not_failure() {
        let d = DetectedClis {
            gcloud: CliState {
                installed: true,
                logged_in: false,
                app_default: false,
            },
            ..DetectedClis::default()
        };
        let plan = auth_plan(&d);
        let v = plan.iter().find(|p| p.provider == "vertex").unwrap();
        assert!(!v.ready);
        assert!(v.detail.contains("gcloud auth application-default login"));
    }

    #[test]
    fn zc_cloud_cli_auth_missing_cli_still_planned_not_ready() {
        let d = DetectedClis::default();
        let plan = auth_plan(&d);
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(|p| !p.ready));
        assert!(ready_providers(&d).is_empty());
    }

    #[test]
    fn zc_cloud_cli_auth_vertex_needs_adc_not_just_login() {
        // `gcloud auth login` alone is insufficient for SDK calls
        let d = DetectedClis {
            gcloud: CliState {
                installed: true,
                logged_in: true,
                app_default: false,
            },
            ..DetectedClis::default()
        };
        let v = auth_plan(&d)
            .into_iter()
            .find(|p| p.provider == "vertex")
            .unwrap();
        assert!(!v.ready);
    }
}
