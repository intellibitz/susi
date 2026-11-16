use super::*;
use crate::susi_core::{
    plane_bus::{topics, PlaneBus, PlaneHandler},
    provider::{BoxFuture, Provider},
    scoped_tools::{current, with_scope, ScopedTool, ScopedToolProvider, ToolScope},
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

struct SessionTools {
    tools: Vec<ScopedTool>,
    label: &'static str,
    calls: AtomicUsize,
}

impl ScopedToolProvider for SessionTools {
    fn tools(&self) -> &[ScopedTool] {
        &self.tools
    }
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn call(
        &self,
        name: &str,
        args: &Value,
        _: &Path,
        _: Option<&str>,
        _: Option<crate::susi_core::progress::Cancel>,
        _: crate::susi_core::progress::Cancel,
    ) -> Result<String, String> {
        assert_eq!(name, "session_weather");
        assert_eq!(args, &json!({"city":"Paris"}));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.label.into())
    }
}

struct ToolBus;
impl PlaneHandler for ToolBus {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        let scope = current().expect("explicit session scope reaches the local bus");
        match topic {
            topics::TOOLS_LIST => Ok(json!(scope.tools().iter().map(|tool| json!({
                "name":tool.name, "description":tool.description, "input_schema":tool.input_schema
            })).collect::<Vec<_>>())),
            topics::TOOLS_EXISTS => {
                Ok(json!({"exists":scope.resolve(payload["name"].as_str().unwrap()).is_some()}))
            }
            topics::TOOLS_EXECUTE => scope
                .call(
                    payload["name"].as_str().unwrap(),
                    &payload["args"],
                    Path::new(payload["workspace"].as_str().unwrap()),
                    None,
                )
                .map(|text| json!({"text":text})),
            _ => panic!("unexpected tool bus operation {topic}"),
        }
    }
}

struct LocalProvider;
impl Provider for LocalProvider {
    fn name(&self) -> &str {
        "ollama-scoped-fixture"
    }
    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(true) })
    }
    fn generate(&self, _: &str) -> BoxFuture<'_, EaiResult<String>> {
        panic!("tool chat required")
    }
    fn generate_with_chat(
        &self,
        _: &str,
        chat: Option<&Value>,
    ) -> BoxFuture<'_, EaiResult<String>> {
        let chat = chat.unwrap();
        assert_eq!(
            chat["tools"][0]["function"]["parameters"]["required"],
            json!(["city"])
        );
        let reply = format!(
            "{}{}",
            crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS,
            json!({"content":"checking", "tool_calls":[{"name":"session_weather", "arguments":{"city":"Paris"}}]})
        );
        Box::pin(async { Ok(reply) })
    }
    fn embed(&self, _: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        panic!("no embedding")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn scoped_provider_executes_local_bus_calls_and_preserves_caller_ownership() {
    const CHILD: &str = "SUSI_SCOPED_INFERENCE_FIXTURE";
    if std::env::var_os(CHILD).is_none() {
        // Global bus/provider registrations and config are confined to a fresh
        // process, with no inherited credentials, providers, or board target.
        let root =
            std::env::temp_dir().join(format!("susi-scoped-inference-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".susi-agent-root"), "scoped-inference-fixture").unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(root.join("board.git"))
            .status()
            .unwrap()
            .success());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "engines::runtime::scoped_tests::scoped_provider_executes_local_bus_calls_and_preserves_caller_ownership", "--nocapture"])
            .env_clear().env(CHILD, "1").env("HOME", &root).env("SUSI_HOME", &root)
            .env("TMPDIR", &root).env("SUSI_BOARD_REMOTE", root.join("board.git"))
            .current_dir(&root).output().unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    assert_eq!(
        std::env::vars()
            .filter(|(key, _)| key.ends_with("API_KEY"))
            .count(),
        0
    );
    assert!(std::env::current_dir()
        .unwrap()
        .join(".susi-agent-root")
        .is_file());
    assert!(std::env::current_dir()
        .unwrap()
        .join("board.git/HEAD")
        .is_file());
    assert_eq!(
        std::env::var_os("SUSI_BOARD_REMOTE").unwrap(),
        std::env::current_dir()
            .unwrap()
            .join("board.git")
            .as_os_str()
    );
    PlaneBus::global().register_prefix("tools.", Arc::new(ToolBus));
    crate::susi_core::registry::CapabilityRegistry::global().register_provider(LocalProvider);
    let workspace = std::env::current_dir().unwrap();
    for label in ["session A", "session B"] {
        let owner = Arc::new(SessionTools {
            tools: vec![ScopedTool {
                name: "session_weather".into(),
                description: "weather in a city".into(),
                input_schema: json!({"type":"object", "required":["city"], "properties":{"city":{"type":"string"}}}),
                required_actions: vec![],
            }],
            label,
            calls: AtomicUsize::new(0),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let scope = ToolScope::new(owner.clone(), cancel.clone());
        with_scope(Some(scope), || {
            let streamed = std::cell::RefCell::new(String::new());
            let reason = |chat| {
                GemiEngine::reason_internal(
                    "weather in Paris",
                    &workspace,
                    false,
                    &|text| streamed.borrow_mut().push_str(&text),
                    None,
                    Some("ollama-scoped-fixture"),
                    None,
                    &|_| {},
                    chat,
                )
            };
            let answer = reason(None);
            assert!(answer.contains(label), "{answer}");
            assert!(!answer.contains(crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS));
            assert!(streamed.borrow().contains(label));
            assert!(!streamed
                .borrow()
                .contains(crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS));
            assert_eq!(owner.calls.load(Ordering::SeqCst), 1);
            let caller = json!({"messages":[{"role":"user", "content":"weather in Paris"}], "tools":[{"type":"function", "function":{"name":"session_weather", "parameters":{"required":["city"]}}}]});
            assert!(reason(Some(caller))
                .starts_with(crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS));
            assert_eq!(
                owner.calls.load(Ordering::SeqCst),
                1,
                "caller-owned calls must not execute"
            );
            let local = "<tool_call>{\"name\":\"session_weather\",\"arguments\":{\"city\":\"Paris\"}}</tool_call>";
            assert!(
                GemiEngine::fulfill_tool_calls(local, &workspace, &|_| {}, true).contains(label)
            );
            assert_eq!(owner.calls.load(Ordering::SeqCst), 2);
            cancel.store(true, Ordering::Release);
            assert!(reason(None).contains("cancelled"));
            assert!(
                GemiEngine::fulfill_tool_calls(local, &workspace, &|_| {}, true)
                    .contains("cancelled")
            );
            assert_eq!(owner.calls.load(Ordering::SeqCst), 2);
        });
        assert!(current().is_none());
    }
}
