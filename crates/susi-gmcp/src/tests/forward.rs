use super::*;
use serde_json::json;

#[test]
fn caller_file_paths_are_absolute_resolved_and_typed() {
    let keys = [("out", true), ("file", false)];
    assert_eq!(policy_paths(&json!({}), &keys).unwrap(), json!({}));
    let refused = policy_paths(&json!({"out": "rel.json"}), &keys).unwrap_err();
    assert!(refused.to_string().contains("absolute"), "{refused}");
    assert!(policy_paths(&json!({"file": 3}), &keys).is_err());
    let dir = std::env::temp_dir().join(format!("susi-forward-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("new").join("out.json");
    let resolved = policy_paths(&json!({"out": out.display().to_string()}), &keys).unwrap();
    let text = resolved["out"].as_str().unwrap();
    assert!(Path::new(text).ends_with(Path::new("new").join("out.json")));
    assert_eq!(
        merged(json!({"a": 1, "b": 1}), json!({"b": 2})),
        json!({"a": 1, "b": 2})
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn payload_copies_set_arguments_and_refuses_wrong_types() {
    let keys = [
        ("name", Kind::Str),
        ("live", Kind::Bool),
        ("limit", Kind::U64),
        ("doc", Kind::Object),
        ("args", Kind::StrArray),
    ];
    let sent = payload(
        "act",
        &json!({"name": "n", "live": true, "limit": 3, "doc": {}, "args": ["a"],
            "ignored": 1, "absent": null}),
        &keys,
        json!({"workspace": "/w"}),
    )
    .unwrap();
    assert_eq!(
        sent,
        json!({"action": "act", "name": "n", "live": true, "limit": 3, "doc": {},
            "args": ["a"], "workspace": "/w"})
    );
    for bad in [
        json!({"name": 1}),
        json!({"live": "yes"}),
        json!({"limit": -1}),
        json!({"doc": []}),
        json!({"args": [1]}),
    ] {
        assert!(payload("act", &bad, &keys, json!({})).is_err(), "{bad}");
    }
}

#[test]
fn workspace_defaults_to_the_server_and_admits_granted_directories() {
    let server = std::env::temp_dir();
    assert_eq!(
        policy_workspace(&json!({}), "workspace", &server, false).unwrap(),
        server
    );
    assert!(policy_workspace(&json!({"workspace": 3}), "workspace", &server, false).is_err());
    assert!(
        policy_workspace(&json!({"workspace": "rel"}), "workspace", &server, false)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    let dir = std::env::temp_dir().join(format!("susi-forward-ws-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let resolved = policy_workspace(
        &json!({"workspace": dir.display().to_string()}),
        "workspace",
        &server,
        true,
    )
    .unwrap();
    assert_eq!(
        resolved.canonicalize().unwrap(),
        dir.canonicalize().unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn forward_and_send_format_pretty_json_and_propagate_call_errors() {
    fn ok_call(v: Value) -> Result<Value, String> {
        Ok(json!({ "received": v }))
    }
    fn err_call(_v: Value) -> Result<Value, String> {
        Err("plane rejected call".into())
    }

    let pretty = forward(
        ok_call,
        "ping",
        &json!({"name": "alpha"}),
        &[("name", Kind::Str)],
        json!({"extra": true}),
    )
    .unwrap();
    let decoded: Value = serde_json::from_str(&pretty).unwrap();
    assert_eq!(decoded["received"]["action"], "ping");
    assert_eq!(decoded["received"]["name"], "alpha");
    assert_eq!(decoded["received"]["extra"], true);

    let err = send(err_call, json!({"action": "ping"})).unwrap_err();
    assert!(err.to_string().contains("plane rejected call"));
}

#[test]
fn kind_describe_and_admits_cover_all_five_json_argument_shapes() {
    assert_eq!(Kind::Str.describe(), "a string");
    assert_eq!(Kind::Bool.describe(), "a boolean");
    assert_eq!(Kind::U64.describe(), "a non-negative integer");
    assert_eq!(Kind::Object.describe(), "an object");
    assert_eq!(Kind::StrArray.describe(), "an array of strings");

    assert!(Kind::StrArray.admits(&json!([])));
    assert!(Kind::StrArray.admits(&json!(["a", "b"])));
    assert!(!Kind::StrArray.admits(&json!(["a", 2])));
}
