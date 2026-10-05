//! Portable model-deployment specifications.
//!
//! A [`DeploymentSpec`] is deliberately separate from an ecosystem
//! [`crate::eco_profile::Profile`].  Profiles describe a provider's wire
//! surface; deployment specs describe one selected model and the resources,
//! credentials and placement needed to run it.  The spec is a closed JSON
//! document so exporting and restoring it cannot silently discard settings.

use crate::eco_schema::{valid_id, Issue, Stage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use susi_error::{EaiError, EaiResult};

/// Version tag for portable deployment documents.
pub const DEPLOYMENT_SPEC_VERSION: &str = "deployment-spec/v1";

/// A model-serving endpoint carried by a deployment specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEndpoint {
    /// Stable name used by clients and health checks.
    pub name: String,
    /// Endpoint URL or an explicitly supported local transport URL.
    pub url: String,
    /// Wire protocol spoken by the endpoint (`openai-compatible`, `grpc`, …).
    pub protocol: String,
    /// Model identifier selected at this endpoint.
    pub model: String,
    /// Optional health path relative to the endpoint URL.
    #[serde(default)]
    pub health_path: Option<String>,
}

/// Resource and engine requirements for a deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequirements {
    /// Runtime or serving engine identifier (`vllm`, `kserve`, `sagemaker`, …).
    pub runtime: String,
    /// Optional engine version or compatibility range.
    #[serde(default)]
    pub version: Option<String>,
    /// Minimum CPU allocation, when the target exposes CPU sizing.
    #[serde(default)]
    pub cpu_cores: Option<u32>,
    /// Minimum memory allocation in MiB.
    #[serde(default)]
    pub memory_mib: Option<u64>,
    /// Accelerator kind, such as `nvidia-a100` or `apple-metal`.
    #[serde(default)]
    pub accelerator: Option<String>,
    /// Minimum accelerator memory in MiB.
    #[serde(default)]
    pub accelerator_memory_mib: Option<u64>,
    /// Runtime features that must be enabled by the target.
    #[serde(default)]
    pub features: Vec<String>,
}

/// The non-secret location from which a deployment obtains one credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretStore {
    /// An environment variable supplied by the launch context.
    Environment,
    /// A local file mounted or provisioned by the launch context.
    File,
    /// A Kubernetes Secret object.
    Kubernetes,
    /// HashiCorp Vault.
    Vault,
    /// AWS Secrets Manager.
    AwsSecretsManager,
    /// Azure Key Vault.
    AzureKeyVault,
    /// Google Secret Manager.
    GcpSecretManager,
}

/// A reference to a credential without embedding the credential value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretReference {
    /// Logical name used by the model server.
    pub name: String,
    /// Secret backend holding the value.
    pub store: SecretStore,
    /// Backend-specific key, path or environment-variable name.
    pub key: String,
}

/// One placement constraint that a target must satisfy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementPolicy {
    /// Cloud or on-premises region, when one is required.
    #[serde(default)]
    pub region: Option<String>,
    /// Data-residency requirement, such as `eu-only`.
    #[serde(default)]
    pub residency: Option<String>,
    /// Acceptable availability zones or local topology names.
    #[serde(default)]
    pub zones: Vec<String>,
    /// Required target labels or node attributes.
    #[serde(default)]
    pub required_labels: BTreeMap<String, String>,
}

/// A supported local or cloud deployment target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentTarget {
    /// A local process or local container runtime.
    Local,
    /// Kubernetes and KServe-style serving targets.
    Kubernetes,
    /// Amazon SageMaker.
    AwsSagemaker,
    /// Azure AI Foundry managed model endpoints.
    AzureFoundry,
    /// Google Vertex AI model endpoints.
    GcpVertex,
}

impl DeploymentTarget {
    fn supported_protocols(self) -> &'static [&'static str] {
        match self {
            Self::Local | Self::Kubernetes => {
                &["openai-compatible", "openai-responses", "http-json", "grpc"]
            }
            Self::AwsSagemaker | Self::AzureFoundry | Self::GcpVertex => {
                &["openai-compatible", "http-json"]
            }
        }
    }

    fn supported_secret_stores(self) -> &'static [SecretStore] {
        match self {
            Self::Local => &[
                SecretStore::Environment,
                SecretStore::File,
                SecretStore::Vault,
            ],
            Self::Kubernetes => &[
                SecretStore::Environment,
                SecretStore::Kubernetes,
                SecretStore::Vault,
            ],
            Self::AwsSagemaker => &[SecretStore::Environment, SecretStore::AwsSecretsManager],
            Self::AzureFoundry => &[SecretStore::Environment, SecretStore::AzureKeyVault],
            Self::GcpVertex => &[SecretStore::Environment, SecretStore::GcpSecretManager],
        }
    }

    fn supports_accelerators(self) -> bool {
        !matches!(self, Self::AzureFoundry)
    }
}

/// Complete portable deployment document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentSpec {
    /// Must equal [`DEPLOYMENT_SPEC_VERSION`].
    pub version: String,
    /// Stable deployment identifier.
    pub id: String,
    /// Selected model identifier.
    pub model: String,
    /// One or more serving endpoints for the selected model.
    pub endpoints: Vec<ModelEndpoint>,
    /// Engine and resource requirements.
    pub runtime_requirements: RuntimeRequirements,
    /// References to credentials, never credential values.
    #[serde(default)]
    pub secret_references: Vec<SecretReference>,
    /// All placement constraints that must hold before restore is applied.
    #[serde(default)]
    pub placement_policies: Vec<PlacementPolicy>,
}

fn issue(path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        stage: Stage::Model,
        path: path.into(),
        message: message.into(),
    }
}

/// Validate a deployment document without contacting or changing a target.
#[must_use]
pub fn validate(spec: &DeploymentSpec) -> Vec<Issue> {
    let mut issues = Vec::new();
    if spec.version != DEPLOYMENT_SPEC_VERSION {
        issues.push(issue(
            "/version",
            format!(
                "deployment spec version {:?}, expected {DEPLOYMENT_SPEC_VERSION:?}",
                spec.version
            ),
        ));
    }
    if !valid_id(&spec.id) {
        issues.push(issue(
            "/id",
            format!("deployment id {:?} is not a valid id", spec.id),
        ));
    }
    if spec.model.trim().is_empty() {
        issues.push(issue("/model", "model must not be empty"));
    }
    if spec.endpoints.is_empty() {
        issues.push(issue(
            "/endpoints",
            "deployment declares no model endpoints",
        ));
    }
    for (index, endpoint) in spec.endpoints.iter().enumerate() {
        let path = format!("/endpoints/{index}");
        if endpoint.name.trim().is_empty() {
            issues.push(issue(
                format!("{path}/name"),
                "endpoint name must not be empty",
            ));
        }
        if endpoint.url.trim().is_empty() {
            issues.push(issue(
                format!("{path}/url"),
                "endpoint URL must not be empty",
            ));
        }
        if endpoint.protocol.trim().is_empty() {
            issues.push(issue(
                format!("{path}/protocol"),
                "endpoint protocol must not be empty",
            ));
        }
        if endpoint.model.trim().is_empty() {
            issues.push(issue(
                format!("{path}/model"),
                "endpoint model must not be empty",
            ));
        }
    }
    if spec.runtime_requirements.runtime.trim().is_empty() {
        issues.push(issue(
            "/runtime_requirements/runtime",
            "runtime must not be empty",
        ));
    }
    if spec.runtime_requirements.cpu_cores == Some(0) {
        issues.push(issue(
            "/runtime_requirements/cpu_cores",
            "cpu_cores must be greater than zero",
        ));
    }
    if spec.runtime_requirements.memory_mib == Some(0) {
        issues.push(issue(
            "/runtime_requirements/memory_mib",
            "memory_mib must be greater than zero",
        ));
    }
    if spec.runtime_requirements.accelerator_memory_mib == Some(0) {
        issues.push(issue(
            "/runtime_requirements/accelerator_memory_mib",
            "accelerator_memory_mib must be greater than zero",
        ));
    }
    for (index, feature) in spec.runtime_requirements.features.iter().enumerate() {
        if feature.trim().is_empty() {
            issues.push(issue(
                format!("/runtime_requirements/features/{index}"),
                "runtime feature must not be empty",
            ));
        }
    }
    for (index, secret) in spec.secret_references.iter().enumerate() {
        if secret.name.trim().is_empty() {
            issues.push(issue(
                format!("/secret_references/{index}/name"),
                "secret name must not be empty",
            ));
        }
        if secret.key.trim().is_empty() {
            issues.push(issue(
                format!("/secret_references/{index}/key"),
                "secret key must not be empty",
            ));
        }
    }
    for (index, policy) in spec.placement_policies.iter().enumerate() {
        if policy.region.as_deref().is_some_and(str::is_empty) {
            issues.push(issue(
                format!("/placement_policies/{index}/region"),
                "region must not be empty when provided",
            ));
        }
        if policy.residency.as_deref().is_some_and(str::is_empty) {
            issues.push(issue(
                format!("/placement_policies/{index}/residency"),
                "residency must not be empty when provided",
            ));
        }
    }
    issues
}

/// Return portability findings for a target before any restore or mutation.
#[must_use]
pub fn check_portability(spec: &DeploymentSpec, target: DeploymentTarget) -> Vec<Issue> {
    let mut issues = validate(spec);
    let protocols = target.supported_protocols();
    for (index, endpoint) in spec.endpoints.iter().enumerate() {
        if !protocols.contains(&endpoint.protocol.as_str()) {
            issues.push(issue(
                format!("/endpoints/{index}/protocol"),
                format!(
                    "protocol {:?} is not supported by {:?}; supported protocols: {}",
                    endpoint.protocol,
                    target,
                    protocols.join(", ")
                ),
            ));
        }
    }
    if spec.runtime_requirements.accelerator.is_some() && !target.supports_accelerators() {
        issues.push(issue(
            "/runtime_requirements/accelerator",
            format!(
                "target {:?} does not support accelerator requirements",
                target
            ),
        ));
    }
    for (index, secret) in spec.secret_references.iter().enumerate() {
        if !target.supported_secret_stores().contains(&secret.store) {
            issues.push(issue(
                format!("/secret_references/{index}/store"),
                format!(
                    "secret store {:?} is not supported by target {:?}",
                    secret.store, target
                ),
            ));
        }
    }
    if matches!(target, DeploymentTarget::Local)
        && spec
            .placement_policies
            .iter()
            .any(|policy| policy.region.is_some() || !policy.zones.is_empty())
    {
        issues.push(issue(
            "/placement_policies",
            "local targets cannot satisfy cloud region or availability-zone placement",
        ));
    }
    issues
}

fn issues_error(prefix: &str, issues: &[Issue]) -> EaiError {
    let first = issues
        .first()
        .map(|i| format!("{} {}", i.path, i.message))
        .unwrap_or_else(|| "unknown deployment-spec issue".to_string());
    EaiError::config(format!(
        "{prefix}: {} issue(s); first: {first}",
        issues.len()
    ))
}

/// Serialize a validated deployment spec for storage or transfer.
///
/// # Errors
/// Returns a configuration error when validation fails or serialization is
/// impossible.
pub fn export_spec(spec: &DeploymentSpec) -> EaiResult<String> {
    let issues = validate(spec);
    if !issues.is_empty() {
        return Err(issues_error("deployment spec is not exportable", &issues));
    }
    serde_json::to_string_pretty(spec)
        .map_err(|error| EaiError::config(format!("serialize deployment spec: {error}")))
}

/// Restore and validate a deployment spec without contacting a target.
///
/// # Errors
/// Returns a configuration error for malformed JSON, unknown fields, or an
/// invalid deployment document.
pub fn restore_spec(text: &str) -> EaiResult<DeploymentSpec> {
    let spec: DeploymentSpec = serde_json::from_str(text)
        .map_err(|error| EaiError::config(format!("deployment spec is not valid JSON: {error}")))?;
    let issues = validate(&spec);
    if !issues.is_empty() {
        return Err(issues_error("deployment spec is invalid", &issues));
    }
    Ok(spec)
}

/// Restore only after portability has been checked for the requested target.
/// This function is pure: callers can perform the returned check before they
/// make any target-side change.
///
/// # Errors
/// Returns a configuration error when the document is invalid or contains a
/// setting the target cannot satisfy.
pub fn restore_for_target(text: &str, target: DeploymentTarget) -> EaiResult<DeploymentSpec> {
    let spec = restore_spec(text)?;
    let issues = check_portability(&spec, target);
    if !issues.is_empty() {
        return Err(issues_error(
            "deployment spec is not portable to target",
            &issues,
        ));
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> DeploymentSpec {
        DeploymentSpec {
            version: DEPLOYMENT_SPEC_VERSION.into(),
            id: "demo-deployment".into(),
            model: "acme-model".into(),
            endpoints: vec![ModelEndpoint {
                name: "chat".into(),
                url: "https://models.example.test/v1".into(),
                protocol: "openai-compatible".into(),
                model: "acme-model".into(),
                health_path: Some("/health".into()),
            }],
            runtime_requirements: RuntimeRequirements {
                runtime: "vllm".into(),
                version: Some(">=0.8".into()),
                cpu_cores: Some(4),
                memory_mib: Some(16_384),
                accelerator: Some("nvidia-a100".into()),
                accelerator_memory_mib: Some(40_960),
                features: vec!["continuous-batching".into()],
            },
            secret_references: vec![SecretReference {
                name: "model-token".into(),
                store: SecretStore::Kubernetes,
                key: "serving/model-token".into(),
            }],
            placement_policies: vec![PlacementPolicy {
                region: Some("eu-central".into()),
                residency: Some("eu-only".into()),
                zones: vec!["eu-central-1a".into()],
                required_labels: BTreeMap::from([("accelerator".into(), "a100".into())]),
            }],
        }
    }

    #[test]
    fn deployment_spec_export_restore_preserves_all_portability_fields() {
        let original = spec();
        let text = export_spec(&original).expect("export validates");
        let restored = restore_spec(&text).expect("restore validates");
        assert_eq!(restored, original);
    }

    #[test]
    fn deployment_spec_rejects_unknown_fields_instead_of_dropping_them() {
        let mut doc = serde_json::to_value(spec()).expect("to value");
        doc.as_object_mut()
            .expect("object")
            .insert("placement".into(), serde_json::json!({"region": "us-east"}));
        let text = serde_json::to_string(&doc).expect("to text");
        let error = restore_spec(&text).expect_err("unknown fields must be refused");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn deployment_spec_portability_check_reports_secret_and_placement_mismatch() {
        let mut candidate = spec();
        candidate.secret_references[0].store = SecretStore::Kubernetes;
        let issues = check_portability(&candidate, DeploymentTarget::AwsSagemaker);
        assert!(issues
            .iter()
            .any(|issue| issue.path == "/secret_references/0/store"));
        assert!(restore_for_target(
            &export_spec(&candidate).unwrap(),
            DeploymentTarget::AwsSagemaker
        )
        .is_err());
    }
}
