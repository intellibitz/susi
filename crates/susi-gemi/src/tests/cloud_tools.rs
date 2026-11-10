use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::engines::http_provider::{HttpProvider, InferenceProtocol};
use crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS;
use crate::susi_core::provider::Provider;

#[allow(clippy::type_complexity)]
fn one_shot(response: &'static str) -> (String, Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let request = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&request);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 2048];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + length {
                break;
            }
        }
        *captured.lock().unwrap() = bytes;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .unwrap();
    });
    (format!("http://{address}/v1"), request, server)
}

fn request_json(request: &[u8]) -> Value {
    let split = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    serde_json::from_slice(&request[split + 4..]).unwrap()
}

fn chat() -> Value {
    json!({
        "messages": [
            {"role": "system", "content": "Answer briefly."},
            {"role": "user", "content": "Weather in Paris?"}
        ],
        "tools": [{"type": "function", "function": {
            "name": "get_weather",
            "description": "Read the weather for a city",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
        }}],
        "max_tokens": 7
    })
}

#[test]
fn cloud_provider_tool_contract_openai_preserves_chat_and_returns_caller_calls() {
    let (base, captured, server) = one_shot(
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}]}}]}"#,
    );
    let provider = HttpProvider {
        name: "test-openai".into(),
        api_base: base,
        model: "test-model".into(),
        protocol: InferenceProtocol::OpenAiChat,
        api_key: String::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let reply = runtime
        .block_on(provider.generate_with_chat("fallback", Some(&chat())))
        .unwrap();
    server.join().unwrap();

    let body = request_json(&captured.lock().unwrap());
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][1]["content"], "Weather in Paris?");
    assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(body["max_tokens"], 7);
    assert!(body.get("stream").is_none());
    let envelope: Value =
        serde_json::from_str(reply.strip_prefix(CALLER_TOOL_CALLS).unwrap()).unwrap();
    assert_eq!(envelope["content"], Value::Null);
    assert_eq!(envelope["tool_calls"][0]["name"], "get_weather");
    assert_eq!(envelope["tool_calls"][0]["arguments"]["city"], "Paris");
}

#[test]
fn cloud_provider_legacy_completion_uses_caller_max_tokens() {
    let (base, captured, server) = one_shot(r#"{"choices":[{"text":"ok"}]}"#);
    let provider = HttpProvider {
        name: "test-completions".into(),
        api_base: base,
        model: "test-model".into(),
        protocol: InferenceProtocol::OpenAiCompletions,
        api_key: String::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let reply = runtime
        .block_on(provider.generate_with_chat("fallback", Some(&json!({"max_tokens": 7}))))
        .unwrap();
    server.join().unwrap();

    let body = request_json(&captured.lock().unwrap());
    assert_eq!(body["prompt"], "fallback");
    assert_eq!(body["max_tokens"], 7);
    assert_eq!(reply, "ok");
}

#[test]
fn cloud_provider_tool_contract_anthropic_translates_messages_tools_and_tool_use() {
    let (base, captured, server) = one_shot(
        r#"{"id":"msg_1","content":[{"type":"tool_use","id":"call_2","name":"get_weather","input":{"city":"Paris"}}],"stop_reason":"tool_use"}"#,
    );
    let provider = HttpProvider {
        name: "test-anthropic".into(),
        api_base: base,
        model: "claude-test".into(),
        protocol: InferenceProtocol::Anthropic,
        api_key: "test-key".into(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let reply = runtime
        .block_on(provider.generate_with_chat("fallback", Some(&chat())))
        .unwrap();
    server.join().unwrap();

    let body = request_json(&captured.lock().unwrap());
    assert_eq!(body["model"], "claude-test");
    assert_eq!(body["system"][0]["text"], "Answer briefly.");
    assert_eq!(body["messages"][0]["content"], "Weather in Paris?");
    assert_eq!(body["tools"][0]["name"], "get_weather");
    assert_eq!(body["tools"][0]["input_schema"]["required"][0], "city");
    let envelope: Value =
        serde_json::from_str(reply.strip_prefix(CALLER_TOOL_CALLS).unwrap()).unwrap();
    assert_eq!(envelope["tool_calls"][0]["name"], "get_weather");
    assert_eq!(envelope["tool_calls"][0]["arguments"]["city"], "Paris");
}

#[test]
fn a_reply_cut_at_the_callers_max_tokens_is_marked_for_the_server() {
    use crate::susi_core::plane_bus::gemi::LENGTH_CUT;
    let reply = |protocol, response: &'static str, chat: Value| {
        let (base, _, server) = one_shot(response);
        let provider = HttpProvider {
            name: "test-length".into(),
            api_base: base,
            model: "test-model".into(),
            protocol,
            api_key: "test-key".into(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let text = runtime
            .block_on(provider.generate_with_chat("fallback", Some(&chat)))
            .unwrap();
        server.join().unwrap();
        text
    };
    let openai_cut = r#"{"choices":[{"message":{"role":"assistant","content":"Once upon"},"finish_reason":"length"}]}"#;
    let asked = json!({"messages": [{"role": "user", "content": "story"}], "max_tokens": 2});
    let unasked = json!({"messages": [{"role": "user", "content": "story"}]});
    assert_eq!(
        reply(InferenceProtocol::OpenAiChat, openai_cut, asked.clone()),
        format!("{LENGTH_CUT}Once upon")
    );
    // A provider's own default limit is not the caller's: no mark.
    assert_eq!(
        reply(InferenceProtocol::OpenAiChat, openai_cut, unasked),
        "Once upon"
    );
    let anthropic_cut = r#"{"id":"msg_1","content":[{"type":"text","text":"Once upon"}],"stop_reason":"max_tokens"}"#;
    assert_eq!(
        reply(InferenceProtocol::Anthropic, anthropic_cut, asked),
        format!("{LENGTH_CUT}Once upon")
    );
}
