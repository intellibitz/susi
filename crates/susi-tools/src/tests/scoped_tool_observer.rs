use super::*;
use crate::susi_core::capture::EvidenceSession;
use crate::susi_core::progress::{self, Cancel};
use crate::susi_core::scoped_tools::{
    with_scope, ScopedTool, ScopedToolObserver, ScopedToolProvider, ToolScope,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

const NAME: &str = "zz_owned_observer";
const COMMAND: &str = "/fixture/observer-mcp";

#[derive(Debug, PartialEq)]
enum Event {
    Started(String),
    Finished(Result<String, String>),
    Abandoned,
}

type Events = Arc<Mutex<Vec<Event>>>;

struct Observer {
    events: Events,
    finished: bool,
}

impl ScopedToolObserver for Observer {
    fn finished(&mut self, outcome: &Result<String, String>) {
        assert!(!self.finished, "one typed final outcome per attempt");
        self.finished = true;
        self.events
            .lock()
            .unwrap()
            .push(Event::Finished(outcome.clone()));
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        if !self.finished {
            self.events.lock().unwrap().push(Event::Abandoned);
        }
    }
}

struct Owner {
    tools: Vec<ScopedTool>,
    events: Events,
    closed: AtomicBool,
    calls: AtomicUsize,
}

impl ScopedToolProvider for Owner {
    fn tools(&self) -> &[ScopedTool] {
        &self.tools
    }
    fn observe_call(&self, name: &str) -> Option<Box<dyn ScopedToolObserver>> {
        self.events
            .lock()
            .unwrap()
            .push(Event::Started(name.into()));
        Some(Box::new(Observer {
            events: self.events.clone(),
            finished: false,
        }))
    }
    fn check(&self) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            Err("owner closed".into())
        } else {
            Ok(())
        }
    }
    fn call(
        &self,
        _: &str,
        args: &Value,
        _: &Path,
        _: Option<&str>,
        cancel: Option<Cancel>,
        turn_cancel: Cancel,
    ) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        assert!(
            !self.events.lock().unwrap().is_empty(),
            "observer starts before execution"
        );
        assert_ne!(args["panic"], true, "interrupted provider");
        if args["cancel_child"] == true {
            cancel.unwrap().store(true, Ordering::Release);
        }
        if args["cancel_turn"] == true {
            turn_cancel.store(true, Ordering::Release);
        }
        if args["close_owner"] == true {
            self.closed.store(true, Ordering::Release);
        }
        // MCP interpretation belongs to the concrete provider. Dispatch
        // observes the resulting Err, never parses a response string.
        if let Some(result) = args.get("mcp_result") {
            if result.get("isError").and_then(Value::as_bool) == Some(true) {
                return Err(format!("MCP tool failed: {result}"));
            }
            return Ok(result.to_string());
        }
        Ok(args["text"].as_str().unwrap_or("source evidence").into())
    }
}

fn owner() -> (Arc<Owner>, Arc<ToolScope>, Cancel) {
    let owner = Arc::new(Owner {
        tools: vec![ScopedTool {
            name: NAME.into(),
            description: "observer fixture".into(),
            input_schema: json!({"type":"object"}),
            required_actions: vec![(actions::PROCESS_EXEC.into(), COMMAND.into())],
        }],
        events: Arc::new(Mutex::new(Vec::new())),
        closed: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let turn = Arc::new(AtomicBool::new(false));
    (owner.clone(), ToolScope::new(owner, turn.clone()), turn)
}

fn dispatch(owner: &Owner, workspace: &Path, args: Value, expected: Result<String, String>) {
    let output = ToolRegistry::execute_tool(NAME, &args, workspace);
    assert_eq!(output, expected.clone().unwrap_or_else(|error| error));
    assert_eq!(
        *owner.events.lock().unwrap(),
        vec![Event::Started(NAME.into()), Event::Finished(expected)]
    );
    owner.events.lock().unwrap().clear();
}

#[test]
fn scoped_tool_observer_reports_authority_closed_and_pre_cancel_refusals_once() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let (owner, scope, turn) = owner();
    with_scope(Some(scope), || {
        dispatch(
            &owner,
            workspace,
            json!({}),
            Err("Protocol Error: session tool dispatch requires its bound mission evidence".into()),
        );
        let session = EvidenceSession::new("observer denial", workspace, str::to_owned).unwrap();
        let _bound = EvidenceSession::enter(Some(session.clone()));
        let mac = MacPolicy::global();
        mac.revoke("susi", actions::PROCESS_EXEC, "*").unwrap();
        dispatch(&owner, workspace, json!({}), Err(format!("Governance Violation: MAC DENY: subject `susi` lacks process.exec on {COMMAND} for session tool `{NAME}`")));
        mac.grant("susi", actions::PROCESS_EXEC, COMMAND, Some(60))
            .unwrap();
        dispatch(&owner, workspace, json!({}), Err(format!("Governance Violation: MAC DENY: subject `{}` lacks process.exec on {COMMAND} for session tool `{NAME}`", session.id())));
        owner.closed.store(true, Ordering::Release);
        dispatch(
            &owner,
            workspace,
            json!({}),
            Err("Protocol Error: owner closed".into()),
        );
        owner.closed.store(false, Ordering::Release);
        turn.store(true, Ordering::Release);
        dispatch(
            &owner,
            workspace,
            json!({}),
            Err("Protocol Error: session tool turn was cancelled".into()),
        );
        assert_eq!(owner.calls.load(Ordering::Acquire), 0);
        assert!(session.receipts().is_empty());
    });
}

#[test]
fn scoped_tool_observer_uses_provider_result_after_late_cancel_and_keeps_success_text() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let session = EvidenceSession::new("observer execution", workspace, str::to_owned).unwrap();
    let _bound = EvidenceSession::enter(Some(session.clone()));
    let (owner, scope, turn) = owner();
    let child = Arc::new(AtomicBool::new(false));
    with_scope(Some(scope), || {
        progress::with_cancel(Some(child.clone()), || {
            let text = "Protocol Error: successful source text is not an error";
            dispatch(&owner, workspace, json!({"text":text}), Ok(text.into()));
            let mcp_error =
                json!({"isError":true,"content":[{"type":"text","text":"fixture failure"}]});
            dispatch(
                &owner,
                workspace,
                json!({"mcp_result":mcp_error}),
                Err(format!("Protocol Error: MCP tool failed: {mcp_error}")),
            );
            dispatch(
                &owner,
                workspace,
                json!({"cancel_child":true}),
                Err("Protocol Error: session tool operation was cancelled".into()),
            );
            assert!(!turn.load(Ordering::Acquire));
            child.store(false, Ordering::Release);
            dispatch(
                &owner,
                workspace,
                json!({"cancel_turn":true}),
                Err("Protocol Error: session tool turn was cancelled".into()),
            );
            assert!(!child.load(Ordering::Acquire));
            turn.store(false, Ordering::Release);
            dispatch(
                &owner,
                workspace,
                json!({"close_owner":true}),
                Err("Protocol Error: owner closed".into()),
            );
        })
    });
    assert_eq!(owner.calls.load(Ordering::Acquire), 5);
    assert_eq!(session.receipts().len(), 5);
    assert_eq!(
        session
            .receipts()
            .iter()
            .filter(|receipt| receipt.successful)
            .count(),
        1
    );
}

#[test]
fn scoped_tool_observer_never_classifies_legacy_names_and_drop_sees_unwind() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let session = EvidenceSession::new("observer legacy", workspace, str::to_owned).unwrap();
    let _bound = EvidenceSession::enter(Some(session));
    let (owner, scope, _) = owner();
    ToolRegistry::register_meta_tool(
        ToolRegistry::global(),
        "zz_legacy_observer",
        "legacy result",
        MetaCategory::SystemPrimitive,
        |_, _| Ok("Protocol Error: raw legacy output".into()),
    );
    ToolRegistry::register_meta_tool(
        ToolRegistry::global(),
        "zz_legacy_error",
        "legacy typed failure",
        MetaCategory::SystemPrimitive,
        |_, _| Err(crate::susi_error::EaiError::protocol("legacy failure")),
    );
    let unknown_before = ToolRegistry::execute_tool("zz_missing_observer", &json!({}), workspace);
    assert!(unknown_before.starts_with("[CAPABILITY_GAP]"));
    with_scope(Some(scope.clone()), || {
        assert!(scope.observe_call("unknown").is_none());
        assert_eq!(
            ToolRegistry::execute_tool("zz_legacy_observer", &json!({}), workspace),
            "Protocol Error: raw legacy output"
        );
        assert_eq!(
            ToolRegistry::execute_tool("zz_missing_observer", &json!({}), workspace),
            unknown_before
        );
        assert_eq!(
            ToolRegistry::execute_tool("zz_legacy_error", &json!({}), workspace),
            "Protocol Error: legacy failure"
        );
        assert!(owner.events.lock().unwrap().is_empty());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ToolRegistry::execute_tool(NAME, &json!({"panic":true}), workspace)
        }));
        assert!(panic.is_err());
        assert_eq!(
            *owner.events.lock().unwrap(),
            vec![Event::Started(NAME.into()), Event::Abandoned]
        );
    });
}
