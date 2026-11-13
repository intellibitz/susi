use super::*;

#[test]
fn gmcp_host_dispatch_handles_json_object_argument_without_panicking() {
    let out = GmcpHost::dispatch("unwired_test_tool", r#"{"key":"val"}"#, Path::new("."));
    assert!(!out.is_empty());
}

#[test]
fn gmcp_host_dispatch_falls_back_to_json_string_when_argument_is_plain_text() {
    let out = GmcpHost::dispatch("unwired_test_tool", "plain-text-arg", Path::new("."));
    assert!(!out.is_empty());
}

#[test]
fn gmcp_host_dispatch_handles_empty_argument_cleanly() {
    let out = GmcpHost::dispatch("", "", Path::new("."));
    assert!(!out.is_empty());
}

#[test]
fn susi_error_reexport_constructs_process_error_from_gmcp_root() {
    let err = susi_error::EaiError::process("gmcp root test");
    assert!(err.to_string().contains("gmcp root test"));
}

#[test]
fn gmcp_crate_reexports_embed_install_source_and_protocol_versions() {
    assert_eq!(embed_provider::INSTALL_SOURCE, "embeddings");
    assert!(!protocol::MODERN_REQUEST_VERSIONS.is_empty());
}
