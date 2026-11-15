use super::*;
use serde_json::json;

#[test]
fn mime_base64_and_uri_validators_enforce_rfc_grammars() {
    assert!(is_valid_mime_type("application/json; charset=utf-8"));
    assert!(!is_valid_mime_type("not-a-mime"));
    assert!(is_valid_base64("SGVsbG8="));
    assert!(!is_valid_base64("bad!base64"));
    assert!(is_valid_uri("https://example.com/resource"));
    assert!(!is_valid_uri("relative/path"));
    assert!(!is_valid_uri(""));
}

#[test]
fn canonical_content_block_preserves_extensions_and_converts_to_rmcp() {
    let raw = json!({
        "type": "text",
        "text": "hello world",
        "x_custom_ext": 42,
        "annotations": {
            "audience": ["user"],
            "priority": 0.5
        }
    });
    let block = CanonicalContentBlock::from_wire(&raw).unwrap();
    let round_trip = block.to_wire();
    assert_eq!(round_trip["x_custom_ext"], json!(42));
    assert!(!block.canonical_json().is_empty());
    assert!(block.to_rmcp().is_ok());
}

#[test]
fn canonical_content_block_rejects_bad_priority_audience_and_control_chars() {
    let bad_prio = json!({
        "type": "text",
        "text": "ok",
        "annotations": { "priority": 1.5 }
    });
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_prio),
        Err(ContentError::BadPriority)
    );

    let bad_ctrl = json!({
        "type": "text",
        "text": "hello\u{0007}bell"
    });
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_ctrl),
        Err(ContentError::BadUnicode("text"))
    );
    assert!(ContentError::TooLarge
        .wire_message()
        .contains("bounded-resource"));
}

#[test]
fn validate_payload_checks_resource_uris_and_client_result_content() {
    let bad_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "resources/read",
        "params": { "uri": "not-an-absolute-uri" }
    });
    assert_eq!(
        validate_payload(bad_req.as_object().unwrap()),
        Some((-32602, ContentError::BadUri("params.uri")))
    );

    let bad_roots = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "roots": [{ "uri": "relative/root" }]
        }
    });
    assert_eq!(
        validate_payload(bad_roots.as_object().unwrap()),
        Some((-32600, ContentError::BadUri("result.roots[].uri")))
    );
}

#[test]
fn canonical_content_block_validates_embedded_resource_and_resource_link_icons() {
    let embedded = json!({
        "type": "resource",
        "resource": {
            "uri": "file:///workspace/README.md",
            "mimeType": "text/markdown",
            "text": "# Title\n"
        }
    });
    assert!(CanonicalContentBlock::from_wire(&embedded).is_ok());

    let bad_icon_link = json!({
        "type": "resource_link",
        "uri": "https://example.com/doc",
        "name": "doc",
        "icons": [{
            "src": "https://example.com/icon.png",
            "theme": "neon"
        }]
    });
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_icon_link),
        Err(ContentError::BadIcon)
    );
}
