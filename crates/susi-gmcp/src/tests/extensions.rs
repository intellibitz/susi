use super::*;

#[test]
fn register_validates_namespacing_and_prevents_duplicate_collisions() {
    let mut reg = ExtensionRegistry::new();
    assert_eq!(
        reg.register("unnamespaced", ExtensionStatus::Experimental, 1, Map::new()),
        Err(ExtensionError::NotNamespaced("unnamespaced".into()))
    );
    reg.register(
        "io.susi/custom",
        ExtensionStatus::Experimental,
        2,
        Map::new(),
    )
    .unwrap();
    assert_eq!(
        reg.register("io.susi/custom", ExtensionStatus::Official, 2, Map::new()),
        Err(ExtensionError::Collision("io.susi/custom".into()))
    );
    assert_eq!(
        reg.status("io.susi/custom"),
        Some(ExtensionStatus::Experimental)
    );
}

#[test]
fn set_enabled_and_capability_map_omit_disabled_and_include_version_above_one() {
    let mut reg = ExtensionRegistry::shipped();
    reg.register("io.susi/v2ext", ExtensionStatus::Official, 2, Map::new())
        .unwrap();
    let caps = reg.capability_map();
    assert!(caps.contains_key("io.modelcontextprotocol/tasks"));
    assert_eq!(caps["io.susi/v2ext"]["version"], Value::from(2));

    assert!(reg.set_enabled("io.susi/v2ext", false));
    assert!(!reg.set_enabled("io.susi/missing", false));
    assert!(!reg.capability_map().contains_key("io.susi/v2ext"));
    assert!(reg.status_report().is_array());
}

#[test]
fn negotiate_separates_active_and_unknown_extensions_and_rejects_downgrade() {
    let mut reg = ExtensionRegistry::new();
    reg.register("io.susi/ext", ExtensionStatus::Official, 2, Map::new())
        .unwrap();

    let mut peer = ExtensionMap::new();
    let mut v1 = Map::new();
    v1.insert("version".into(), Value::from(1));
    peer.insert("io.susi/ext".into(), v1);
    peer.insert("io.other/unknown".into(), Map::new());

    let downgrade = reg.negotiate(&peer, &["io.susi/ext".to_string()]);
    assert!(matches!(downgrade, Err(ExtensionError::Downgrade { .. })));

    let ok = reg.negotiate(&peer, &[]).unwrap();
    assert!(ok.active.contains_key("io.susi/ext"));
    assert!(ok.unknown.contains_key("io.other/unknown"));
}

#[test]
fn watch_reports_unregistered_official_extensions_and_formats_error_messages() {
    let reg = ExtensionRegistry::shipped();
    let items = reg.watch(OFFICIAL_EXTENSIONS);
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .any(|s| s.contains("io.modelcontextprotocol/ui")));
    assert!(ExtensionError::RequiredUnsupported(vec!["a/b".into()])
        .message()
        .contains("a/b"));
}

#[test]
fn negotiate_fails_closed_when_required_extension_is_disabled_or_missing() {
    let mut reg = ExtensionRegistry::shipped();
    assert!(reg.set_enabled("io.modelcontextprotocol/tasks", false));
    let err = reg
        .negotiate(
            &ExtensionMap::new(),
            &["io.modelcontextprotocol/tasks".to_string()],
        )
        .unwrap_err();
    assert_eq!(
        err,
        ExtensionError::RequiredUnsupported(vec!["io.modelcontextprotocol/tasks".to_string()])
    );
    assert_eq!(reg.status("io.susi/nonexistent"), None);
}
