use super::*;
use crate::susi_core::capture::EvidenceSession;
use crate::susi_core::scoped_tools::{with_scope, ScopedTool, ScopedToolProvider, ToolScope};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const NAME: &str = "zz_session_owned_probe";
const COMMAND: &str = "/fixture/session-mcp";

struct Owner {
    tools: Vec<ScopedTool>,
    marker: String,
    closed: AtomicBool,
    calls: AtomicUsize,
}

impl ScopedToolProvider for Owner {
    fn tools(&self) -> &[ScopedTool] {
        &self.tools
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
        _: &Value,
        _: &Path,
        mission: Option<&str>,
        _: Option<crate::susi_core::progress::Cancel>,
        _: crate::susi_core::progress::Cancel,
    ) -> Result<String, String> {
        let bound = EvidenceSession::current().expect("scope must carry its mission evidence");
        assert_eq!(mission, Some(bound.id()));
        self.calls.fetch_add(1, Ordering::AcqRel);
        Ok(self.marker.clone())
    }
}

fn owner(marker: &str) -> (Arc<Owner>, Arc<ToolScope>) {
    let owner = Arc::new(Owner {
        tools: vec![ScopedTool {
            name: NAME.into(),
            description: marker.into(),
            input_schema: json!({"type":"object","properties":{"literal":{"type":"string"}},"required":["literal"]}),
            required_actions: vec![(actions::PROCESS_EXEC.into(), COMMAND.into())],
        }],
        marker: marker.into(),
        closed: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let scope = ToolScope::new(owner.clone(), Arc::new(AtomicBool::new(false)));
    (owner, scope)
}

#[test]
fn scoped_tools_preserve_schema_host_and_mission_revocation_and_evidence() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let session = EvidenceSession::new("scoped authority", workspace, str::to_owned).unwrap();
    let _evidence = EvidenceSession::enter(Some(session.clone()));
    let (owner, scope) = owner("source evidence");
    let mac = MacPolicy::global();
    mac.revoke("susi", actions::PROCESS_EXEC, "*").unwrap();
    mac.grant("susi", actions::PROCESS_EXEC, COMMAND, Some(60))
        .unwrap();
    assert!(!ToolRegistry::exists(NAME));
    with_scope(Some(scope.clone()), || {
        let catalog = ToolRegistry::list_tools();
        assert_eq!(
            catalog
                .iter()
                .find(|tool| tool.name == NAME)
                .unwrap()
                .input_schema
                .as_ref(),
            Some(&scope.tools()[0].input_schema)
        );
        assert!(ToolRegistry::exists(NAME));
        // An exact host grant cannot silently become a wildcard mission grant.
        let denied = ToolRegistry::execute_tool(NAME, &json!({}), workspace);
        assert!(
            denied.contains("MAC DENY") && denied.contains(session.id()),
            "{denied}"
        );
        assert_eq!(owner.calls.load(Ordering::Acquire), 0);
        mac.grant(session.id(), actions::PROCESS_EXEC, COMMAND, Some(60))
            .unwrap();
        assert_eq!(
            ToolRegistry::execute_tool(NAME, &json!({}), workspace),
            "source evidence"
        );
        assert_eq!(session.receipts().len(), 1);
        assert!(session.receipts()[0].successful);
        mac.revoke(session.id(), actions::PROCESS_EXEC, COMMAND)
            .unwrap();
        assert!(ToolRegistry::execute_tool(NAME, &json!({}), workspace).contains("MAC DENY"));
        mac.grant(session.id(), actions::PROCESS_EXEC, COMMAND, Some(60))
            .unwrap();
        mac.revoke("susi", actions::PROCESS_EXEC, COMMAND).unwrap();
        assert!(ToolRegistry::execute_tool(NAME, &json!({}), workspace).contains("MAC DENY"));
        assert_eq!(owner.calls.load(Ordering::Acquire), 1);
        assert_eq!(session.receipts().len(), 1);
    });
    assert!(!ToolRegistry::list_tools()
        .iter()
        .any(|tool| tool.name == NAME));
    assert!(crate::susi_core::registry::CapabilityRegistry::global()
        .get_tool(NAME)
        .is_none());
}

#[test]
fn scoped_tools_same_workspace_workers_keep_catalog_and_receipts_separate() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let first = EvidenceSession::new("first", workspace, str::to_owned).unwrap();
    let second = EvidenceSession::new("second", workspace, str::to_owned).unwrap();
    let (_, first_scope) = owner("first");
    let (_, second_scope) = owner("second");
    let _latest_workspace_session = EvidenceSession::activate(&second);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|threads| {
        for (session, scope, marker) in [
            (first.clone(), first_scope, "first"),
            (second.clone(), second_scope, "second"),
        ] {
            let barrier = &barrier;
            threads.spawn(move || {
                let _evidence = EvidenceSession::enter(Some(session));
                with_scope(Some(scope), || {
                    barrier.wait();
                    let catalog = ToolRegistry::list_tools();
                    assert_eq!(
                        catalog
                            .iter()
                            .find(|tool| tool.name == NAME)
                            .unwrap()
                            .description,
                        marker
                    );
                    let results = crate::susi_core::parallel::map((0..8).collect(), |_| {
                        ToolRegistry::execute_tool(NAME, &json!({}), workspace)
                    });
                    assert_eq!(results, vec![marker; 8]);
                });
            });
        }
    });
    for session in [&first, &second] {
        assert_eq!(session.receipts().len(), 8);
        assert!(session.receipts().iter().all(|receipt| receipt.successful));
    }
}

#[test]
fn scoped_tools_closed_scope_refuses_global_fallback_and_legacy_stays_available() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let session = EvidenceSession::new("closed owner", workspace, str::to_owned).unwrap();
    let _evidence = EvidenceSession::enter(Some(session));
    ToolRegistry::register_meta_tool(
        ToolRegistry::global(),
        NAME,
        "global fallback",
        MetaCategory::SystemPrimitive,
        |_, _| Ok("global".into()),
    );
    let (owner, scope) = owner("session");
    with_scope(Some(scope), || {
        let row = ToolRegistry::list_tools()
            .into_iter()
            .find(|tool| tool.name == NAME)
            .unwrap();
        assert_eq!(row.description, "session");
        assert_eq!(row.input_schema, Some(owner.tools[0].input_schema.clone()));
        assert_eq!(
            ToolRegistry::execute_tool(NAME, &json!({}), workspace),
            "session"
        );
        owner.closed.store(true, Ordering::Release);
        assert!(ToolRegistry::execute_tool(NAME, &json!({}), workspace).contains("owner closed"));
        assert!(!ToolRegistry::exists(NAME));
        assert!(ToolRegistry::list_tools().is_empty());
    });
    assert_eq!(owner.calls.load(Ordering::Acquire), 1);
    assert_eq!(
        ToolRegistry::execute_tool(NAME, &json!({}), workspace),
        "global"
    );
}

#[test]
fn scoped_tools_refuse_missing_or_mismatched_mission_instead_of_workspace_fallback() {
    let Some(home) = crate::private_config_fixture() else {
        return;
    };
    let workspace = home.parent().unwrap();
    let session = EvidenceSession::new("another session", workspace, str::to_owned).unwrap();
    let _latest = EvidenceSession::activate(&session);
    let (owner, scope) = owner("must not execute");
    with_scope(Some(scope), || {
        assert!(ToolRegistry::execute_tool(NAME, &json!({}), workspace)
            .contains("bound mission evidence"));
        let _bound = EvidenceSession::enter(Some(session.clone()));
        assert!(ToolRegistry::execute_tool(NAME, &json!({}), &home).contains("workspace differs"));
    });
    assert_eq!(owner.calls.load(Ordering::Acquire), 0);
    assert!(session.receipts().is_empty());
}
