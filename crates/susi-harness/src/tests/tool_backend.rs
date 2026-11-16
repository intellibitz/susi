use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Mutex;

struct ResourcePeer {
    resources: Mutex<BTreeMap<String, Value>>,
    calls: Vec<String>,
}

impl Transport for ResourcePeer {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        let method = request["method"].as_str().unwrap();
        self.calls.push(method.to_owned());
        let result = match method {
            "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{}}),
            "resources/read" => {
                let uri = request["params"]["uri"].as_str().unwrap();
                self.resources.lock().unwrap().get(uri).cloned().unwrap()
            }
            "tools/call" => {
                assert_eq!(request["params"]["name"], UPLOAD_TOOL);
                assert_eq!(request["params"]["arguments"]["action"], "discard");
                json!({"content":[{"type":"text","text":"{}"}]})
            }
            _ => panic!("unexpected method: {method}"),
        };
        Ok(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
    }
}

fn resource_response(body: &Value, legacy: bool) -> (McpClient<ResourcePeer>, Value) {
    let root = format!("susi-resource://{}/result", "a".repeat(64));
    let text = serde_json::to_string(body).unwrap();
    let manifest = json!({"format":"susi-json-chunks-v1","mimeType":"application/json",
        "size":text.len(),"sha256":digest(text.as_bytes()),"chunks":1,
        "chunkUriTemplate":format!("{root}/{{index}}")})
    .to_string();
    let resources = BTreeMap::from([
        (
            root.clone(),
            json!({"contents":[{"uri":root,"mimeType":"application/json","text":manifest}]}),
        ),
        (
            format!("{root}/0"),
            json!({"contents":[{"uri":format!("{root}/0"),"mimeType":"text/plain","text":text}]}),
        ),
    ]);
    let block = if legacy {
        json!({"type":"resource","resource":{"uri":root,"mimeType":"application/json","text":manifest}})
    } else {
        json!({"type":"resource_link","uri":root,"mimeType":"application/json","size":manifest.len(),
            "_meta":{"susi/resource":"json-chunks-v1","susi/content":"mcp-tool-result"}})
    };
    let outer = json!({"content":[block],"isError":false});
    (
        McpClient::new(ResourcePeer {
            resources: Mutex::new(resources),
            calls: Vec::new(),
        }),
        outer,
    )
}

#[test]
fn modern_and_legacy_resources_preserve_text_and_default_error_flags() {
    for legacy in [false, true] {
        for text in ["", "{\"x\":1}", "\u{03bb}\n"] {
            let body = json!({"content":[{"type":"text","text":text}]});
            let (mut client, outer) = resource_response(&body, legacy);
            assert_eq!(hydrate(&mut client, outer).unwrap(), body);
            assert_eq!(client.transport().calls.last().unwrap(), "tools/call");
        }
    }
}

#[test]
fn resource_error_flags_require_booleans_and_matching_effective_values() {
    for legacy in [false, true] {
        for flag in [json!(true), json!("false"), Value::Null] {
            let body = json!({"content":[{"type":"text","text":"error"}],"isError":flag});
            let (mut client, outer) = resource_response(&body, legacy);
            assert!(hydrate(&mut client, outer).is_err());
            assert!(!client
                .transport()
                .calls
                .iter()
                .any(|call| call == "tools/call"));
        }
        let body = json!({"content":[{"type":"text","text":"error"}],"isError":true});
        let (mut client, mut outer) = resource_response(&body, legacy);
        outer["isError"] = json!(true);
        assert_eq!(hydrate(&mut client, outer).unwrap(), body);
        let (mut client, mut outer) = resource_response(&body, legacy);
        outer["isError"] = json!("true");
        assert!(hydrate(&mut client, outer).is_err());
    }
}

#[test]
fn resources_reject_wrong_uri_and_corrupted_content_without_replay() {
    for legacy in [false, true] {
        for corrupt_uri in [false, true] {
            let body = json!({"content":[{"type":"text","text":"original"}]});
            let (mut client, outer) = resource_response(&body, legacy);
            let root = format!("susi-resource://{}/result/0", "a".repeat(64));
            let mut resources = client.transport().resources.lock().unwrap();
            let chunk = &mut resources.get_mut(&root).unwrap()["contents"][0];
            if corrupt_uri {
                chunk["uri"] = json!(format!("susi-resource://{}/result/0", "b".repeat(64)));
            } else {
                chunk["text"] = json!("corrupted");
            }
            drop(resources);
            assert!(hydrate(&mut client, outer).is_err());
            assert!(!client
                .transport()
                .calls
                .iter()
                .any(|call| call == "tools/call"));
        }
    }
}

#[test]
fn structured_results_keep_empty_text_and_tool_errors_fail_closed() {
    assert_eq!(
        json_result(
            json!({"structuredContent":{"text":""},"isError":false}),
            None
        )
        .unwrap(),
        json!({"text":""})
    );
    assert_eq!(
        json_result(json!({"content":[{"type":"text","text":"[1,null]"}]}), None).unwrap(),
        json!([1, null])
    );
    for result in [
        json!({"structuredContent":{},"isError":true}),
        json!({"structuredContent":{},"isError":"false"}),
        json!({"structuredContent":[]}),
        json!({"content":[]}),
    ] {
        assert!(json_result(result, None).is_err());
    }
}

#[test]
fn newest_structured_content_accepts_all_json_values_without_weakening_older_revisions() {
    for value in [
        Value::Null,
        json!(false),
        json!(42),
        json!("text"),
        json!([1, null]),
    ] {
        let result = json!({"structuredContent":value,"content":[]});
        assert_eq!(
            json_result(result.clone(), Some("2026-07-28")).unwrap(),
            value
        );
        for revision in [
            None,
            Some("2024-11-05"),
            Some("2025-03-26"),
            Some("2025-06-18"),
            Some("2025-11-25"),
        ] {
            assert!(json_result(result.clone(), revision).is_err());
        }
    }
}
