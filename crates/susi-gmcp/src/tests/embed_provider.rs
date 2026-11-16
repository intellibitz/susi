use super::*;

#[test]
fn local_embed_provider_reports_canonical_name_and_downcasts_via_as_any() {
    let provider = LocalEmbedProvider;
    assert_eq!(provider.name(), "susi-fastembed");
    assert!(provider
        .as_any()
        .downcast_ref::<LocalEmbedProvider>()
        .is_some());
}

#[test]
fn local_embed_provider_generate_rejects_chat_prompts_as_embedding_only() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(LocalEmbedProvider.generate("hello"))
        .unwrap_err();
    assert!(err.to_string().contains("embedding-only provider"));
}

#[test]
fn local_embed_provider_health_check_returns_boolean_status() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let healthy = rt.block_on(LocalEmbedProvider.is_healthy()).unwrap();
    assert_eq!(healthy, !load_failed());
}

struct FakeEmbedPlug;
impl crate::susi_core::plane_bus::PlaneHandler for FakeEmbedPlug {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        match topic {
            TOPIC_EMBED_TEXTS => {
                let n = payload["texts"].as_array().map_or(0, Vec::len);
                Ok(json!({ "vectors": vec![vec![0.5_f32, 0.25]; n] }))
            }
            TOPIC_EMBED_INSTALL => Ok(json!({ "message": "already installed" })),
            other => Err(format!("fake plug: {other}")),
        }
    }
}

#[test]
fn a_wired_plug_answers_embed_and_install_through_the_bus() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(TOPIC_EMBED_TEXTS, std::sync::Arc::new(FakeEmbedPlug));
    PlaneBus::global().register(TOPIC_EMBED_INSTALL, std::sync::Arc::new(FakeEmbedPlug));
    assert!(plug_wired());
    let vectors = embed(vec!["a".into(), "b".into()]).unwrap();
    assert_eq!(vectors, vec![vec![0.5, 0.25], vec![0.5, 0.25]]);
    assert_eq!(embed_one("a").unwrap(), vec![0.5, 0.25]);
    assert_eq!(install_on_request().unwrap(), "already installed");
    assert!(embed(Vec::new()).unwrap().is_empty());
}

#[test]
fn register_local_embed_provider_is_idempotent_and_exposes_install_source() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    register_into(&registry);
    register_into(&registry);
    assert!(registry.get_provider("susi-fastembed").is_some());
    assert_eq!(INSTALL_SOURCE, "embeddings");
}

#[test]
fn registered_local_embed_provider_rejects_generate_through_capability_registry() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    register_into(&registry);
    let provider = registry
        .get_provider("susi-fastembed")
        .expect("registered susi-fastembed provider");
    assert_eq!(provider.name(), "susi-fastembed");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt.block_on(provider.generate("chat prompt")).unwrap_err();
    assert!(err.to_string().contains("embedding-only provider"));
}
