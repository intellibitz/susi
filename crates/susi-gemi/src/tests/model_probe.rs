use super::*;
use crate::susi_core::provider::BoxFuture;
use std::any::Any;
use susi_error::EaiError;

struct Probe {
    name: &'static str,
    fail: bool,
}

impl Provider for Probe {
    fn name(&self) -> &str {
        self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, Result<bool>> {
        panic!("a model probe must not add a health request")
    }

    fn generate(&self, prompt: &str) -> BoxFuture<'_, Result<String>> {
        assert_eq!(prompt, " \trequest\n", "keep the caller's untrimmed prompt");
        Box::pin(async move {
            assert_eq!(crate::key_arbitration::scope_status(self.name).in_flight, 1);
            if self.fail {
                Err(EaiError::network("probe transport failed"))
            } else {
                Ok("answer github_pat_11ABCdef".into())
            }
        })
    }

    fn embed(&self, _: &str) -> BoxFuture<'_, Result<Vec<f32>>> {
        panic!("a model probe must not request embeddings")
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test]
fn validation_precedes_provider_resolution_and_preserves_construction_errors() {
    for prompt in ["", " \t\n"] {
        let err = run::<Probe>(prompt, "unused runtime context", || {
            panic!("invalid prompt must not construct the manager/provider")
        })
        .unwrap_err();
        assert_eq!(err.kind_name(), "Internal");
        assert_eq!(
            err.to_string(),
            "Internal Engine Error: probe prompt must not be empty"
        );
    }
    let err = run::<Probe>("request", "unused runtime context", || {
        Err(EaiError::config("catalog unavailable"))
    })
    .unwrap_err();
    assert_eq!(err.kind_name(), "Config");
    assert_eq!(err.to_string(), "Configuration Error: catalog unavailable");
}

#[test]
fn probes_keep_prompt_redaction_error_mapping_and_permit_lifetime() {
    let _lock = crate::engines::env_test_lock();
    let home = susi_paths::tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", home.path());
    for (name, fail) in [("probe-parity-ok", false), ("probe-parity-error", true)] {
        let result = run(" \trequest\n", "model probe runtime", || {
            Ok(Probe { name, fail })
        });
        assert_eq!(crate::key_arbitration::scope_status(name).in_flight, 0);
        if fail {
            let err = result.unwrap_err();
            assert_eq!(err.kind_name(), "Internal");
            assert_eq!(
                err.to_string(),
                "Internal Engine Error: Network Error: probe transport failed"
            );
        } else {
            let text = result.unwrap();
            assert!(text.starts_with("answer "));
            assert!(!text.contains("github_pat_11ABCdef"));
        }
    }
}
