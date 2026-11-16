use super::completed_response;
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn a_completion_reply_is_bound_to_the_requested_ids_and_consumer_commit() {
    let requested: BTreeSet<_> = ["T-DEPENDENCY-1".into(), "T-DEPENDENCY-2".into()].into();
    let head = "a".repeat(40);
    let reply = json!({"schema":"susi-completed-dependencies-v1","accepted_in":head,"ids":["T-DEPENDENCY-1"]});
    assert_eq!(
        completed_response(&reply, &requested, &head).unwrap(),
        ["T-DEPENDENCY-1".into()].into()
    );
    for delta in [
        json!({"schema":"other"}),
        json!({"accepted_in":"b".repeat(40)}),
        json!({"ids":["T-FOREIGN-1"]}),
        json!({"ids":[true]}),
        json!({"ids":null}),
    ] {
        let mut invalid = reply.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(delta.as_object().unwrap().clone());
        assert!(completed_response(&invalid, &requested, &head).is_none());
    }
    let absent = json!({"schema":"susi-completed-dependencies-v1","accepted_in":head,"ids":[]});
    assert!(completed_response(&absent, &requested, &head)
        .unwrap()
        .is_empty());
}
