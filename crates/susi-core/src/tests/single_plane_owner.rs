use super::tests::{tempdir, Echo, Reject};
use super::*;

#[test]
fn single_owner_ignores_prefixes_and_refuses_ambiguous_exact_owners() {
    let dir = tempdir("single-owner");
    let caller = IpcPlaneBus::with_rendezvous(dir.clone());
    let owner = IpcPlaneBus::with_rendezvous(dir.clone());
    let topic = "fixture.operator.run";
    caller.register_prefix("fixture.", Arc::new(Echo));
    assert_eq!(
        caller.try_request_exact_once(topic, Value::Null).unwrap(),
        None
    );
    owner.register(topic, Arc::new(Reject));
    assert_eq!(
        caller
            .try_request_exact_once(topic, Value::Null)
            .unwrap_err(),
        "nope"
    );
    caller.register(topic, Arc::new(Echo));
    let error = caller
        .try_request_exact_once(topic, Value::Null)
        .unwrap_err();
    assert!(error.contains("multiple exact plane owners"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_disconnected_registered_owner_is_an_error_not_absence() {
    let dir = tempdir("single-disconnected");
    let caller = IpcPlaneBus::with_rendezvous(dir.clone());
    let topic = "fixture.operator.run";
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    let topics = dir.join("topics");
    std::fs::create_dir_all(&topics).unwrap();
    std::fs::write(
        topics.join(enc(topic)),
        json!({"endpoint":endpoint.to_string(),"key":topic}).to_string(),
    )
    .unwrap();
    assert!(caller
        .try_request_exact_once(topic, Value::Null)
        .unwrap_err()
        .contains("registered plane owner unavailable"));
    let _ = std::fs::remove_dir_all(dir);
}
