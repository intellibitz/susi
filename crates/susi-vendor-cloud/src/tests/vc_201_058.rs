use crate::failover_policy::{
    route, Endpoint, EndpointClass, EndpointState, NoRoute, Privacy, RouteConstraints,
};

fn ep(id: &str, class: EndpointClass, state: EndpointState) -> Endpoint {
    Endpoint {
        id: id.into(),
        class,
        capabilities: vec!["chat".into()],
        est_latency_ms: 50,
        cost_cents: 5,
        state,
    }
}

fn constraints() -> RouteConstraints {
    RouteConstraints {
        needs_capability: "chat".into(),
        privacy: Privacy::CloudOk,
        deadline_ms: 1000,
        budget_cents: 100,
    }
}

#[test]
fn vc_201_058_falls_through_oom_outage_authfail_and_quota() {
    let c = constraints();
    let eps = vec![
        ep("local", EndpointClass::Local, EndpointState::Oom),
        ep("aws", EndpointClass::Cloud, EndpointState::Outage),
        ep("gcp", EndpointClass::Cloud, EndpointState::AuthFailed),
        ep("azure", EndpointClass::Cloud, EndpointState::QuotaExhausted),
        ep("backup", EndpointClass::Cloud, EndpointState::Up),
    ];
    let chosen = route(&c, &eps).unwrap();
    assert_eq!(chosen.id, "backup");
}

#[test]
fn vc_201_058_privacy_constraint_survives_failover() {
    // Local OOM must never leak a LocalOnly request to the cloud.
    let mut c = constraints();
    c.privacy = Privacy::LocalOnly;
    let eps = vec![
        ep("local", EndpointClass::Local, EndpointState::Oom),
        ep("aws", EndpointClass::Cloud, EndpointState::Up),
    ];
    let err = route(&c, &eps).unwrap_err();
    let rendered = err.to_string();
    assert!(rendered.contains("local OOM"));
    assert!(rendered.contains("privacy requires local"));
}

#[test]
fn vc_201_058_deadline_and_budget_are_preserved_not_dropped() {
    let mut c = constraints();
    c.deadline_ms = 10;
    c.budget_cents = 1;
    let eps = vec![ep("local", EndpointClass::Local, EndpointState::Up)];
    let err = route(&c, &eps).unwrap_err();
    // Both constraints violated — deadline is the first refusal recorded.
    assert!(err.to_string().contains("deadline"));

    // Endpoint that fits deadline but not budget → budget refusal.
    let eps = vec![Endpoint {
        cost_cents: 200,
        ..ep("local", EndpointClass::Local, EndpointState::Up)
    }];
    let mut c = constraints();
    c.budget_cents = 50;
    assert!(route(&c, &eps).unwrap_err().to_string().contains("budget"));
}

#[test]
fn vc_201_058_missing_capability_and_no_route_lists_every_attempt() {
    let c = constraints(); // needs "chat"
    let mut incapable = ep("local", EndpointClass::Local, EndpointState::Up);
    incapable.capabilities = vec!["embed".into()];
    let eps = vec![
        incapable,
        ep("aws", EndpointClass::Cloud, EndpointState::Outage),
    ];
    let NoRoute { refusals } = route(&c, &eps).unwrap_err();
    assert_eq!(refusals.len(), 2);
    assert_eq!(refusals[0].reason, "missing capability");
    assert_eq!(refusals[1].reason, "provider outage");
}
