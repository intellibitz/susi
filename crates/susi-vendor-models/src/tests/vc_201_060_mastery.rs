//! Mastery checks for VC-201-060: round-trip a portable deployment spec
//! (endpoints, runtime requirements, secret references, placement policies)
//! between local and cloud targets, with compatibility checks surfacing
//! nonportable settings before any changes.
//!
//! Every test name starts `vc_201_060_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-vendor-models vc_201_060`.

use crate::deployment_spec::{
    self, DeploymentSpec, DeploymentTarget, ModelEndpoint, PlacementPolicy, RuntimeRequirements,
    SecretReference, SecretStore, DEPLOYMENT_SPEC_VERSION,
};
use std::collections::BTreeMap;

fn spec() -> DeploymentSpec {
    DeploymentSpec {
        version: DEPLOYMENT_SPEC_VERSION.into(),
        id: "portable-chat".into(),
        model: "acme-chat".into(),
        endpoints: vec![ModelEndpoint {
            name: "chat".into(),
            url: "https://models.example.test/v1".into(),
            protocol: "openai-compatible".into(),
            model: "acme-chat".into(),
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
fn vc_201_060_mastery_export_restore_roundtrips_portable_fields() {
    let original = spec();
    let exported = deployment_spec::export_spec(&original).expect("export validates");
    let restored = deployment_spec::restore_spec(&exported).expect("restore validates");
    assert_eq!(restored, original);
    let json = serde_json::to_value(restored).expect("restored spec serializes");
    let object = json.as_object().expect("deployment spec is an object");
    for field in [
        "endpoints",
        "runtime_requirements",
        "secret_references",
        "placement_policies",
    ] {
        assert!(object.contains_key(field), "round-trip lost {field}");
    }
}

#[test]
fn vc_201_060_mastery_restore_refuses_unknown_fields() {
    let mut document = serde_json::to_value(spec()).expect("spec serializes");
    document
        .as_object_mut()
        .expect("spec object")
        .insert("placement".into(), serde_json::json!({"region": "us-east"}));
    let exported = serde_json::to_string(&document).expect("document serializes");
    let error = deployment_spec::restore_spec(&exported)
        .expect_err("restore must not silently discard deployment settings");
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn vc_201_060_mastery_portability_checks_run_before_target_restore() {
    let exported = deployment_spec::export_spec(&spec()).expect("export validates");
    let issues = deployment_spec::check_portability(&spec(), DeploymentTarget::AwsSagemaker);
    assert!(issues
        .iter()
        .any(|issue| issue.path == "/secret_references/0/store"));
    let error = deployment_spec::restore_for_target(&exported, DeploymentTarget::AwsSagemaker)
        .expect_err("Kubernetes secret cannot be applied to SageMaker");
    assert!(error.to_string().contains("not portable"));
}

#[test]
fn vc_201_060_mastery_supported_cloud_restore_is_clean() {
    let mut candidate = spec();
    candidate.secret_references[0].store = SecretStore::AwsSecretsManager;
    candidate.placement_policies[0].zones.clear();
    let exported = deployment_spec::export_spec(&candidate).expect("export validates");
    let restored = deployment_spec::restore_for_target(&exported, DeploymentTarget::AwsSagemaker)
        .expect("AWS-compatible spec restores after portability check");
    assert_eq!(restored, candidate);
}
