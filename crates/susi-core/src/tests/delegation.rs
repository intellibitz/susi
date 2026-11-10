//! Recursive driver/engine delegation (Mandate 73, T-CLAUDERECURSE-1).

use crate::capability_bus::{
    CapabilityBus, CapabilityDescriptor, CapabilityError, CapabilityInvocation, CapabilityRequest,
    CapabilitySurface,
};
use crate::delegation::{
    DelegationAuthority, DelegationEnvelope, DelegationError, DelegationHop, DelegationLimits,
    DelegationRequest, DelegationScope, ENVELOPE_KEY_PURPOSE, ENVELOPE_SCHEMA, MAX_DEPTH_CEILING,
    MAX_LEDGER_ENTRIES, MAX_SIGNED_LIFETIME_MS,
};
use serde_json::json;
use std::collections::BTreeSet;

fn perms(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|p| (*p).to_string()).collect()
}

fn hop(component: &str, capability: &str) -> DelegationHop {
    DelegationHop::new(component, capability).unwrap()
}

fn root(budget: u64, limits: DelegationLimits) -> DelegationScope {
    DelegationScope::root(
        hop("ceo-session", "root"),
        perms(&["fs.read", "net.fetch", "a2a.run"]),
        budget,
        None,
        limits,
    )
    .unwrap()
}

fn ask(component: &str, capability: &str, permissions: &[&str], budget: u64) -> DelegationRequest {
    DelegationRequest {
        hop: hop(component, capability),
        permissions: perms(permissions),
        budget_micros: budget,
        timeout_ms: 60_000,
    }
}

#[test]
fn driver_and_engine_roles_nest_and_keep_provenance() {
    let session = root(1_000, DelegationLimits::default());
    // dot drives opus; opus (an engine to dot) drives carbon; carbon calls
    // back into dot for a different capability: dot is now an engine.
    let opus = session
        .delegate(ask("dot", "plan", &["fs.read", "a2a.run"], 600))
        .unwrap();
    let carbon = opus
        .delegate(ask("opus", "build", &["fs.read"], 300))
        .unwrap();
    let dot_again = carbon
        .delegate(ask("dot", "review", &["fs.read"], 100))
        .unwrap();
    assert_eq!(dot_again.depth(), 3);
    assert_eq!(
        dot_again.path(),
        vec![
            hop("ceo-session", "root"),
            hop("dot", "plan"),
            hop("opus", "build"),
            hop("dot", "review"),
        ]
    );
    assert_eq!(session.remaining_micros(), 400);
    assert_eq!(opus.remaining_micros(), 300);
    assert_eq!(carbon.remaining_micros(), 200);
}

#[test]
fn a_child_cannot_amplify_permissions() {
    let session = root(1_000, DelegationLimits::default());
    let narrow = session
        .delegate(ask("dot", "plan", &["fs.read"], 10))
        .unwrap();
    let err = narrow
        .delegate(ask("opus", "fetch", &["fs.read", "net.fetch", "admin"], 1))
        .unwrap_err();
    assert_eq!(
        err,
        DelegationError::PermissionAmplification {
            missing: vec!["admin".into(), "net.fetch".into()],
        }
    );
    // A refusal leaves the parent unchanged.
    assert_eq!(narrow.remaining_micros(), 10);
    assert!(narrow
        .delegate(ask("opus", "read", &["fs.read"], 1))
        .is_ok());
}

#[test]
fn budgets_are_reserved_from_the_parent_and_unspent_budget_returns() {
    let session = root(100, DelegationLimits::default());
    let child = session.delegate(ask("dot", "plan", &[], 70)).unwrap();
    assert_eq!(session.remaining_micros(), 30);
    assert_eq!(
        session.delegate(ask("grok", "plan", &[], 31)).unwrap_err(),
        DelegationError::BudgetExceeded {
            requested_micros: 31,
            available_micros: 30,
        }
    );
    child.charge(20).unwrap();
    assert_eq!(child.spent_micros(), 20);
    assert_eq!(
        child.charge(51).unwrap_err(),
        DelegationError::BudgetExceeded {
            requested_micros: 51,
            available_micros: 50,
        }
    );
    // A grandchild holds its parent alive; its unspent budget returns first.
    let grandchild = child.delegate(ask("opus", "build", &[], 40)).unwrap();
    grandchild.charge(15).unwrap();
    drop(grandchild);
    assert_eq!(child.remaining_micros(), 35);
    drop(child);
    // 100 - 20 spent by the child - 15 spent by the grandchild.
    assert_eq!(session.remaining_micros(), 65);
}

#[test]
fn depth_is_bounded_and_inherited() {
    let limits = DelegationLimits {
        max_depth: 2,
        max_children: 8,
    };
    let session = root(10, limits);
    let one = session.delegate(ask("a", "x", &[], 0)).unwrap();
    let two = one.delegate(ask("b", "x", &[], 0)).unwrap();
    assert_eq!(two.limits(), limits);
    assert_eq!(
        two.delegate(ask("c", "x", &[], 0)).unwrap_err(),
        DelegationError::DepthExceeded { depth: 3, limit: 2 }
    );
    assert!(matches!(
        DelegationScope::root(
            hop("s", "root"),
            Vec::new(),
            0,
            None,
            DelegationLimits {
                max_depth: MAX_DEPTH_CEILING + 1,
                max_children: 1,
            },
        ),
        Err(DelegationError::InvalidLimits(_))
    ));
}

#[test]
fn re_entering_a_hop_on_the_ancestry_is_a_cycle() {
    let session = root(10, DelegationLimits::default());
    let plan = session.delegate(ask("dot", "plan", &[], 0)).unwrap();
    let build = plan.delegate(ask("opus", "build", &[], 0)).unwrap();
    let err = build.delegate(ask("dot", "plan", &[], 0)).unwrap_err();
    assert!(
        matches!(&err, DelegationError::Cycle { hop: h, path } if *h == hop("dot", "plan") && path.len() == 3),
        "{err}"
    );
    // Siblings may repeat a hop: only the ancestry counts.
    assert!(session.delegate(ask("dot", "plan", &[], 0)).is_ok());
}

#[test]
fn fan_out_is_bounded_over_the_scope_lifetime() {
    let session = root(
        10,
        DelegationLimits {
            max_depth: 4,
            max_children: 2,
        },
    );
    for name in ["a", "b"] {
        drop(session.delegate(ask(name, "x", &[], 1)).unwrap());
    }
    // Dropped children still count: a loop cannot keep re-delegating.
    assert_eq!(
        session.delegate(ask("c", "x", &[], 1)).unwrap_err(),
        DelegationError::FanOutExceeded { limit: 2 }
    );
    // A budget refusal does not use up a slot.
    let other = root(
        1,
        DelegationLimits {
            max_depth: 4,
            max_children: 1,
        },
    );
    assert!(matches!(
        other.delegate(ask("a", "x", &[], 2)),
        Err(DelegationError::BudgetExceeded { .. })
    ));
    assert!(other.delegate(ask("a", "x", &[], 1)).is_ok());
}

#[test]
fn cancelling_a_scope_cancels_every_descendant_but_not_its_parent() {
    let session = root(10, DelegationLimits::default());
    let driver = session.delegate(ask("dot", "plan", &[], 0)).unwrap();
    let engine = driver.delegate(ask("opus", "build", &[], 0)).unwrap();
    let sibling = session.delegate(ask("grok", "audit", &[], 0)).unwrap();
    driver.cancel();
    assert!(engine.is_cancelled());
    assert_eq!(
        engine.check().unwrap_err(),
        DelegationError::Cancelled("dot:plan".into())
    );
    assert!(matches!(
        engine.delegate(ask("carbon", "test", &[], 0)),
        Err(DelegationError::Cancelled(_))
    ));
    assert!(!session.is_cancelled());
    assert!(sibling.check().is_ok());
}

#[test]
fn a_child_deadline_never_outlives_its_parent() {
    let session = DelegationScope::root(
        hop("s", "root"),
        Vec::new(),
        0,
        Some(1),
        DelegationLimits::default(),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(matches!(
        session.check(),
        Err(DelegationError::DeadlineExceeded(_))
    ));
    assert!(matches!(
        session.delegate(ask("a", "x", &[], 0)),
        Err(DelegationError::DeadlineExceeded(_))
    ));

    let roomy = DelegationScope::root(
        hop("s", "root"),
        Vec::new(),
        0,
        Some(50),
        DelegationLimits::default(),
    )
    .unwrap();
    let child = roomy
        .delegate(DelegationRequest {
            timeout_ms: 3_600_000,
            ..ask("a", "x", &[], 0)
        })
        .unwrap();
    assert!(child.remaining_time().unwrap() <= std::time::Duration::from_millis(50));
}

#[test]
fn identities_are_validated() {
    for (component, capability) in [("", "x"), ("a", " "), ("a\nb", "x")] {
        assert!(matches!(
            DelegationHop::new(component, capability),
            Err(DelegationError::InvalidIdentity(_))
        ));
    }
    assert!(DelegationHop::new("a".repeat(257), "x").is_err());
    let session = root(1, DelegationLimits::default());
    let forged = DelegationRequest {
        hop: DelegationHop {
            component: String::new(),
            capability: "x".into(),
        },
        ..ask("a", "x", &[], 0)
    };
    assert!(matches!(
        session.delegate(forged),
        Err(DelegationError::InvalidIdentity(_))
    ));
}

/// Two processes of one host derive the same envelope key from the shared
/// host secret; another host's secret differs.
const HOST_SECRET: [u8; 32] = [42; 32];

fn driver_under(session: &DelegationScope, budget: u64) -> DelegationScope {
    session
        .delegate(DelegationRequest {
            timeout_ms: 60_000,
            ..ask("dot", "plan", &["fs.read"], budget)
        })
        .unwrap()
}

/// Sign `envelope` exactly as a holder of the host key would.
fn sign_as_key_holder(envelope: &mut DelegationEnvelope) {
    let key = crate::susi_config::cluster_key::hmac_sha256(&HOST_SECRET, ENVELOPE_KEY_PURPOSE);
    envelope.signature = None;
    envelope.signature = Some(crate::susi_config::cluster_key::hmac_sha256_hex(
        &key,
        &envelope.signed_payload().unwrap(),
    ));
}

#[test]
fn signed_envelopes_continue_the_nesting_in_another_process() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let receiver = DelegationAuthority::from_secret(&HOST_SECRET);
    let session = root(500, DelegationLimits::default());
    let driver = driver_under(&session, 200);
    let envelope = issuer.issue(&driver).unwrap();
    assert_eq!(envelope.schema, ENVELOPE_SCHEMA);
    assert_eq!(envelope.id.as_ref().map(String::len), Some(32));
    assert!(envelope.signature.is_some());
    let lifetime = envelope.expires_unix_ms.unwrap() - now_ms();
    assert!(lifetime <= 60_000, "expiry follows the scope deadline");

    let wire = serde_json::to_string(&envelope).unwrap();
    let received: DelegationEnvelope = serde_json::from_str(&wire).unwrap();
    let remote = receiver.admit(&received).unwrap();
    assert_eq!(remote.path(), driver.path());
    assert_eq!(remote.depth(), 1);
    assert_eq!(remote.permissions(), &perms(&["fs.read"]));
    assert_eq!(remote.remaining_micros(), 200);
    assert!(remote.remaining_time().unwrap() <= std::time::Duration::from_millis(60_000));
    // The receiver still cannot amplify, overspend or re-enter an ancestor.
    assert!(matches!(
        remote.delegate(ask("opus", "x", &["net.fetch"], 0)),
        Err(DelegationError::PermissionAmplification { .. })
    ));
    assert!(matches!(
        remote.delegate(ask("opus", "x", &[], 201)),
        Err(DelegationError::BudgetExceeded { .. })
    ));
    assert!(matches!(
        remote.delegate(ask("ceo-session", "root", &[], 0)),
        Err(DelegationError::Cycle { .. })
    ));
    let engine = remote
        .delegate(ask("opus", "build", &["fs.read"], 50))
        .unwrap();
    assert_eq!(engine.depth(), 2);
    // The issuing process keeps the driver's reservation while the envelope
    // is live: the session cannot hand those 200 micros out again.
    drop(driver);
    assert_eq!(session.remaining_micros(), 300);
}

/// One way of altering an issued envelope.
type Tamper = dyn Fn(&mut DelegationEnvelope);

#[test]
fn a_tampered_forged_or_unsigned_envelope_is_refused() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let receiver = DelegationAuthority::from_secret(&HOST_SECRET);
    let envelope = issuer
        .issue(&driver_under(&root(500, DelegationLimits::default()), 200))
        .unwrap();
    let tampered: Vec<(&str, Box<Tamper>)> = vec![
        (
            "wider permissions",
            Box::new(|e| e.permissions.push("net.fetch".into())),
        ),
        ("larger budget", Box::new(|e| e.budget_micros = 10_000)),
        (
            "later expiry",
            Box::new(|e| e.expires_unix_ms = e.expires_unix_ms.map(|t| t + 1)),
        ),
        ("other id", Box::new(|e| e.id = Some("00".repeat(16)))),
        (
            "shorter path",
            Box::new(|e| {
                e.path.remove(0);
            }),
        ),
        (
            "deeper limits",
            Box::new(|e| e.limits.max_depth = MAX_DEPTH_CEILING),
        ),
        (
            "garbled signature",
            Box::new(|e| e.signature = Some("zz".into())),
        ),
    ];
    for (what, change) in tampered {
        let mut forged = envelope.clone();
        change(&mut forged);
        assert!(
            matches!(receiver.admit(&forged), Err(DelegationError::Unverified(_))),
            "{what} was admitted"
        );
    }
    let mut unsigned = envelope.clone();
    unsigned.signature = None;
    assert!(matches!(
        receiver.admit(&unsigned),
        Err(DelegationError::Unverified(_))
    ));
    let other_host = DelegationAuthority::from_secret(&[43; 32]);
    assert!(matches!(
        other_host.admit(&envelope),
        Err(DelegationError::Unverified(_))
    ));
    // The untouched envelope still verifies.
    assert!(receiver.admit(&envelope).is_ok());
}

#[test]
fn an_expired_envelope_is_refused() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let session = DelegationScope::root(
        hop("s", "root"),
        Vec::new(),
        10,
        Some(30),
        DelegationLimits::default(),
    )
    .unwrap();
    let envelope = issuer.issue(&session).unwrap();
    assert!(envelope.expires_unix_ms.unwrap() <= now_ms() + 30);
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(matches!(
        DelegationAuthority::from_secret(&HOST_SECRET).admit(&envelope),
        Err(DelegationError::Expired { .. })
    ));
    // No deadline still means a bounded signed lifetime.
    let open = issuer.issue(&root(1, DelegationLimits::default())).unwrap();
    assert!(open.expires_unix_ms.unwrap() <= now_ms() + MAX_SIGNED_LIFETIME_MS);
}

#[test]
fn replaying_an_envelope_continues_the_same_level() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let receiver = DelegationAuthority::from_secret(&HOST_SECRET);
    let envelope = issuer
        .issue(&driver_under(&root(500, DelegationLimits::default()), 200))
        .unwrap();
    let first = receiver.admit(&envelope).unwrap();
    let spent = first.delegate(ask("opus", "build", &[], 150)).unwrap();
    let replay = receiver.admit(&envelope).unwrap();
    // A replay does not reset the budget: only 50 is left for anyone.
    assert_eq!(replay.remaining_micros(), 50);
    assert!(matches!(
        replay.delegate(ask("grok", "build", &[], 51)),
        Err(DelegationError::BudgetExceeded { .. })
    ));
    drop(spent);
    assert_eq!(replay.remaining_micros(), 200);
}

#[test]
fn revoking_an_envelope_cancels_its_level_and_everything_under_it() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let receiver = DelegationAuthority::from_secret(&HOST_SECRET);
    let envelope = issuer
        .issue(&driver_under(&root(500, DelegationLimits::default()), 200))
        .unwrap();
    let id = envelope.id.clone().unwrap();
    let level = receiver.admit(&envelope).unwrap();
    let engine = level.delegate(ask("opus", "build", &[], 10)).unwrap();
    assert!(receiver.revoke(&id));
    assert!(engine.is_cancelled());
    assert!(matches!(
        receiver.admit(&envelope),
        Err(DelegationError::Cancelled(_))
    ));
    assert!(!receiver.revoke(&"ff".repeat(16)));
    // Revocation is per process: the issuer did not run it.
    assert!(issuer.admit(&envelope).is_ok());
}

#[test]
fn even_a_key_holder_cannot_admit_a_malformed_envelope() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let good = issuer.issue(&root(5, DelegationLimits::default())).unwrap();
    let mut wrong_schema = good.clone();
    wrong_schema.schema = "other/v9".into();
    let mut empty = good.clone();
    empty.path.clear();
    let mut looped = good.clone();
    looped.path.push(hop("ceo-session", "root"));
    let mut too_deep = good.clone();
    too_deep.limits.max_depth = 1;
    too_deep.path.push(hop("a", "x"));
    too_deep.path.push(hop("b", "x"));
    let mut greedy = good.clone();
    greedy.limits.max_depth = MAX_DEPTH_CEILING + 1;
    let mut bad_permission = good;
    bad_permission.permissions.push(String::new());
    for (mut envelope, what) in [
        (wrong_schema, "schema"),
        (empty, "empty"),
        (looped, "cycle"),
        (too_deep, "depth"),
        (greedy, "limits"),
        (bad_permission, "permission"),
    ] {
        envelope.id = Some(format!("{:032x}", what.len()));
        sign_as_key_holder(&mut envelope);
        assert!(
            DelegationAuthority::from_secret(&HOST_SECRET)
                .admit(&envelope)
                .is_err(),
            "{what} envelope was admitted"
        );
    }
    // Unknown fields are refused rather than silently ignored.
    let extra = json!({
        "schema": ENVELOPE_SCHEMA,
        "path": [{"component": "a", "capability": "root"}],
        "permissions": [],
        "budget_micros": 0,
        "limits": {"max_depth": 4, "max_children": 4},
        "admin": true
    });
    assert!(serde_json::from_value::<DelegationEnvelope>(extra).is_err());
}

#[test]
fn the_ledger_is_bounded() {
    let issuer = DelegationAuthority::from_secret(&HOST_SECRET);
    let session = root(0, DelegationLimits::default());
    for _ in 0..MAX_LEDGER_ENTRIES {
        issuer.issue(&session).unwrap();
    }
    assert_eq!(
        issuer.issue(&session).unwrap_err(),
        DelegationError::LedgerFull {
            limit: MAX_LEDGER_ENTRIES
        }
    );
}

#[test]
fn the_host_authority_signs_with_the_shared_policy_key() {
    let host = DelegationAuthority::host().unwrap();
    let envelope = host
        .issue(&driver_under(&root(10, DelegationLimits::default()), 5))
        .unwrap();
    assert!(host.admit(&envelope).is_ok());
    // A key derived for another purpose does not verify envelopes.
    let policy_key = crate::mac_policy::MacPolicy::global()
        .derive_key(b"some-other-token/v1")
        .unwrap();
    assert!(matches!(
        DelegationAuthority::from_secret(&policy_key).admit(&envelope),
        Err(DelegationError::Unverified(_))
    ));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn bus() -> CapabilityBus {
    let mut bus = CapabilityBus::new();
    for (id, permission, cost) in [
        ("plan", "a2a.run", 100),
        ("build", "fs.read", 60),
        ("fetch", "net.fetch", 10),
    ] {
        bus.register(CapabilityDescriptor::new(
            id,
            CapabilitySurface::Adapter,
            "invoke",
            permission,
            1_000,
            cost,
        ))
        .unwrap();
    }
    bus
}

fn request(id: &str, subject: &str, permissions: &[&str], budget: u64) -> CapabilityRequest {
    CapabilityRequest::new(
        id,
        subject,
        permissions.iter().map(|p| (*p).to_string()),
        1_000,
        budget,
    )
}

fn invocation(cost: u64) -> CapabilityInvocation {
    CapabilityInvocation {
        output: json!({"ok": true}),
        elapsed_ms: 1,
        cost_micros: cost,
    }
}

#[test]
fn bus_handlers_dispatch_nested_capabilities_under_their_scope() {
    let bus = bus();
    let top = request("plan", "dot", &["a2a.run", "fs.read"], 1_000);
    let receipt = run(&bus, &top, |_, scope| {
        assert_eq!(scope.depth(), 1);
        // The engine dispatches as a driver from inside its own handler.
        let nested = bus
            .dispatch_in(
                scope,
                &request("build", "opus", &["fs.read"], 1_000),
                |_, inner| {
                    assert_eq!(inner.depth(), 2);
                    Ok(invocation(40))
                },
            )
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            nested.delegation_path,
            vec![hop("dot", "root"), hop("dot", "plan"), hop("opus", "build")]
        );
        Ok(invocation(30))
    })
    .unwrap();
    assert_eq!(
        receipt.delegation_path,
        vec![hop("dot", "root"), hop("dot", "plan")]
    );
    assert_eq!(receipt.cost_micros, 30);
}

/// A top-level dispatch whose handler receives its scope (`dispatch` keeps
/// the single-argument handler every existing caller uses).
fn run<F>(
    bus: &CapabilityBus,
    request: &CapabilityRequest,
    handler: F,
) -> Result<crate::capability_bus::CapabilityReceipt, CapabilityError>
where
    F: FnOnce(&CapabilityDescriptor, &DelegationScope) -> Result<CapabilityInvocation, String>,
{
    let root = CapabilityBus::root_scope(request)?;
    bus.dispatch_in(&root, request, handler)
}

#[test]
fn nested_dispatch_cannot_amplify_overspend_or_recurse() {
    let bus = &bus();
    // Permission the driver was never given.
    let err = run(
        bus,
        &request("plan", "dot", &["a2a.run"], 1_000),
        |_, scope| match bus.dispatch_in(
            scope,
            &request("fetch", "opus", &["net.fetch"], 1_000),
            |_, _| Ok(invocation(1)),
        ) {
            Err(CapabilityError::Delegation(DelegationError::PermissionAmplification {
                missing,
            })) => Err(format!("amplify:{}", missing.join(","))),
            other => Err(format!("unexpected {other:?}")),
        },
    )
    .unwrap_err();
    assert_eq!(
        err,
        CapabilityError::HandlerFailed("amplify:net.fetch".into())
    );

    // Plan's ceiling is 100. The nested build reserves and spends 60 of it,
    // so plan's own cost of 50 no longer fits: 110 > 100.
    let err = run(
        bus,
        &request("plan", "dot", &["a2a.run", "fs.read"], 1_000),
        |_, scope| {
            bus.dispatch_in(
                scope,
                &request("build", "opus", &["fs.read"], 1_000),
                |_, _| Ok(invocation(60)),
            )
            .map_err(|e| format!("{e:?}"))?;
            Ok(invocation(50))
        },
    )
    .unwrap_err();
    assert_eq!(
        err,
        CapabilityError::BudgetExceeded {
            cost_micros: 110,
            limit_micros: 100,
        }
    );

    // A capability re-entering itself under the same subject is a cycle.
    let err = run(
        bus,
        &request("build", "opus", &["fs.read"], 1_000),
        |_, scope| match bus.dispatch_in(
            scope,
            &request("build", "opus", &["fs.read"], 1_000),
            |_, _| Ok(invocation(0)),
        ) {
            Err(CapabilityError::Delegation(DelegationError::Cycle { .. })) => Err("cycle".into()),
            other => Err(format!("unexpected {other:?}")),
        },
    )
    .unwrap_err();
    assert_eq!(err, CapabilityError::HandlerFailed("cycle".into()));
}

#[test]
fn a_result_finished_after_cancellation_is_not_success() {
    let bus = bus();
    let session = DelegationScope::root(
        hop("ceo-session", "root"),
        perms(&["fs.read"]),
        1_000,
        None,
        DelegationLimits::default(),
    )
    .unwrap();
    let err = bus
        .dispatch_in(
            &session,
            &request("build", "opus", &["fs.read"], 1_000),
            |_, _| {
                session.cancel();
                Ok(invocation(1))
            },
        )
        .unwrap_err();
    assert_eq!(
        err,
        CapabilityError::Delegation(DelegationError::Cancelled("ceo-session:root".into()))
    );
    // Nothing more is admitted under the cancelled session.
    assert!(matches!(
        bus.dispatch_in(
            &session,
            &request("build", "opus", &["fs.read"], 1_000),
            |_, _| Ok(invocation(1)),
        ),
        Err(CapabilityError::Delegation(DelegationError::Cancelled(_)))
    ));
}

#[test]
fn receipts_without_a_delegation_path_keep_their_old_wire_shape() {
    let bus = bus();
    let receipt = bus
        .dispatch(&request("fetch", "dot", &["net.fetch"], 10), |_| {
            Ok(invocation(5))
        })
        .unwrap();
    let wire = serde_json::to_value(&receipt).unwrap();
    assert_eq!(
        wire["delegation_path"],
        json!([
            {"component": "dot", "capability": "root"},
            {"component": "dot", "capability": "fetch"}
        ])
    );
    // Older receipts (no field) still deserialize.
    let old = json!({
        "capability_id": "fetch",
        "surface": "adapter",
        "subject": "dot",
        "elapsed_ms": 1,
        "cost_micros": 5,
        "output": null
    });
    let parsed: crate::capability_bus::CapabilityReceipt = serde_json::from_value(old).unwrap();
    assert!(parsed.delegation_path.is_empty());
}
