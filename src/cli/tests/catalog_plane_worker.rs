use super::*;

fn record(status: &str) -> Value {
    json!({"id":"run-id","agent":"fixture","workspace":"ws","prompt":"test",
        "adapter":{"kind":"command","program":"git","args":["--version"]},
        "status":status,"created_at":1,"updated_at":2,"pid":null,
        "remote_id":null,"remote_url":null,"exit_code":0,"error":null})
}

#[test]
fn registered_worker_accepts_the_durable_result_with_owner_metadata() {
    let mut committed = record("succeeded");
    committed["board_claim"] = json!(19);
    let mut receipt = committed.clone();
    receipt["updated_at"] = json!(1);
    assert!(confirm_worker_receipt(&committed, &receipt).is_ok());
    receipt["board_claim"] = json!(20);
    assert!(confirm_worker_receipt(&committed, &receipt).is_err());
}

#[test]
fn registered_worker_refuses_wrong_identity_state_or_an_uncommitted_job() {
    let committed = record("succeeded");
    for (field, wrong) in [
        ("id", "other"),
        ("agent", "other"),
        ("workspace", "elsewhere"),
        ("status", "running"),
    ] {
        let mut receipt = record("succeeded");
        receipt[field] = json!(wrong);
        assert!(
            confirm_worker_receipt(&committed, &receipt).is_err(),
            "{field}"
        );
    }
    let queued = record("queued");
    assert!(confirm_worker_receipt(&queued, &queued).is_err());
}
