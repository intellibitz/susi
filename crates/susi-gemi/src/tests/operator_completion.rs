use super::completion_count;
use serde_json::json;

#[test]
fn retained_completion_counts_require_bound_typed_success() {
    let head = "a".repeat(40);
    let reply =
        json!({"schema":"susi-completed-report-v1","accepted_in":head,"verified_receipts":2});
    assert_eq!(
        completion_count(&json!({"structuredContent":reply}).to_string(), &head),
        Some(2)
    );
    assert_eq!(
        completion_count(
            &json!({"content":[{"type":"text","text":reply.to_string()}]}).to_string(),
            &head
        ),
        Some(2)
    );
    for invalid in [
        json!({"structuredContent":reply,"isError":true}),
        json!({"structuredContent":reply,"isError":"false"}),
        json!({"structuredContent":{"schema":"susi-completed-report-v1","accepted_in":"other","verified_receipts":0}}),
        json!({"structuredContent":{"schema":"other","accepted_in":head,"verified_receipts":0}}),
        json!({"structuredContent":{"schema":"susi-completed-report-v1","accepted_in":head,"verified_receipts":true}}),
        json!({"structuredContent":{"schema":"susi-completed-report-v1","accepted_in":head,"verified_receipts":-1}}),
        json!({"content":[]}),
    ] {
        assert_eq!(completion_count(&invalid.to_string(), &head), None);
    }
    assert_eq!(completion_count(&json!({"structuredContent":{"schema":"susi-completed-report-v1","accepted_in":head,"verified_receipts":0}}).to_string(), &head), Some(0));
}
