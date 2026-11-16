use super::*;
use serde_json::json;

fn catalog() -> Value {
    json!([{"name":"mcp_weather", "description":"Read weather", "input_schema":{
        "type":"object", "required":["city"], "properties":{"city":{"type":"string"}},
        "additionalProperties":false
    }}])
}

#[test]
fn scoped_catalog_keeps_schema_and_conversation_without_owning_caller_tools() {
    let chat = json!({"messages":[{"role":"system", "content":"retain me"}], "max_tokens":19});
    let (injected, owned) = scoped_chat_request(Some(chat.clone()), &catalog(), &[], "weather");
    assert!(owned);
    let injected = injected.unwrap();
    assert_eq!(injected["messages"], chat["messages"]);
    assert_eq!(injected["max_tokens"], 19);
    assert_eq!(
        injected["tools"][0]["function"]["parameters"],
        catalog()[0]["input_schema"]
    );
    let (unchanged, owned) =
        scoped_chat_request(Some(injected.clone()), &json!([]), &[], "weather");
    assert!(!owned);
    assert_eq!(unchanged, Some(injected));
    assert_eq!(
        scoped_chat_request(None, &catalog(), &[], "unrelated"),
        (None, true)
    );
}

#[test]
fn scoped_catalog_includes_every_admitted_tool_without_lexical_overlap() {
    let session: Vec<_> = (0..12)
        .map(|index| crate::susi_core::scoped_tools::ScopedTool {
            name: format!("session_{index}"),
            description: "measurement".into(),
            input_schema: json!({"type":"object", "required":[format!("value_{index}")]}),
            required_actions: vec![],
        })
        .collect();
    let (chat, owned) = scoped_chat_request(None, &catalog(), &session, "unrelated synonyms");
    assert!(owned);
    let chat = chat.unwrap();
    assert_eq!(chat["tools"].as_array().unwrap().len(), session.len());
    for (index, tool) in session.iter().enumerate() {
        assert_eq!(chat["tools"][index]["function"]["name"], tool.name);
        assert_eq!(
            chat["tools"][index]["function"]["parameters"],
            tool.input_schema
        );
    }
    let prompt_only = prompt_with_owned_tools("unrelated synonyms", Some(&chat));
    for tool in &session {
        assert!(prompt_only.contains(&tool.name));
        assert!(prompt_only.contains(&tool.input_schema.to_string()));
    }
    let (unchanged, owned) =
        scoped_chat_request(Some(chat.clone()), &catalog(), &session, "weather");
    assert!(!owned);
    assert_eq!(unchanged, Some(chat));
}

#[test]
fn owned_provider_markers_and_local_markup_normalize_to_the_same_call() {
    let calls = json!([{"name":"mcp_weather", "arguments":{"city":"Paris"}}]);
    let marker = format!(
        "{}{}",
        crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS,
        json!({"content":"checking", "tool_calls":calls})
    );
    let local = format!("checking<tool_call>{}</tool_call>", calls[0]);
    assert_eq!(
        extract_owned_tool_calls(&marker),
        extract_owned_tool_calls(&local)
    );
    assert_eq!(caller_tool_reply(&marker), marker);
    assert_eq!(extract_tool_calls(&marker), (marker.clone(), vec![]));
    for invalid in [
        "not-json".to_string(),
        json!({"tool_calls":[{"arguments":{}}]}).to_string(),
    ] {
        let marker = format!(
            "{}{invalid}",
            crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS
        );
        assert_eq!(extract_owned_tool_calls(&marker), (marker, vec![]));
    }
}

#[test]
fn scoped_stream_holds_fragmented_markers_and_retains_ordinary_text() {
    let marker = format!(
        "{}{}",
        crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS,
        json!({"content":"checking", "tool_calls":[{"name":"mcp_weather", "arguments":{}}]})
    );
    for chunk_size in 1..marker.len() {
        let mut filter = ToolCallStreamFilter::with_owned_calls(true);
        for chunk in marker.as_bytes().chunks(chunk_size) {
            assert_eq!(filter.feed(std::str::from_utf8(chunk).unwrap()), "");
        }
        assert_eq!(filter.finish(), "checking");
    }
    let mut filter = ToolCallStreamFilter::with_owned_calls(true);
    assert_eq!(filter.feed("ordinary answer"), "ordinary answer");
    assert_eq!(filter.finish(), "");
    let mut caller = ToolCallStreamFilter::default();
    assert_eq!(caller.feed(&marker), marker);
}
