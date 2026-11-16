use super::*;

#[test]
fn scorecard_lifecycle_and_batch_git_contracts() {
    let evs = [
        json!({"phase":"released","at_unix":150,"cost":2,"cost_unit":"usd"}),
        json!({"phase":"promoted","at_unix":160,"cost":0.5,"cost_unit":"usd"}),
        json!({"phase":"defect","at_unix":125,"outcome":"failed"}),
    ];
    let t1 = json!({"id":"T-1","created_unix":100,"closed":{"at_unix":130}});
    let ok = score_task(&t1, &evs, Some(110), Some(&json!({"merged_unix": 140})));
    assert_eq!(
        (&ok["status"], &ok["elapsed_seconds"], &ok["defects"]),
        (&json!("complete"), &json!(60), &json!(1))
    );
    assert_eq!(ok["cost"], json!({"value": 2.5, "unit": "usd"}));
    assert_eq!(
        score_task(&json!({"id":"T-2","created_unix":200}), &[], None, None)["stop_at"],
        "implemented"
    );
    let bad = [json!({"phase":"promoted","at_unix":290})];
    assert_eq!(
        score_task(&json!({"id":"T-3","created_unix":300}), &bad, None, None)["status"],
        "invalid"
    );

    let dir = susi_paths::tempfile::tempdir().unwrap();
    let root = dir.path();
    let done = root.join(".agents/tasks/done");
    std::fs::create_dir_all(&done).unwrap();
    let rec = r#"{"id":"T-G","created_unix":100,"closed":{"at_unix":200}}"#;
    std::fs::write(done.join("T-G.json"), rec).unwrap();
    let env = [
        ("GIT_AUTHOR_DATE", "@150 +0000"),
        ("GIT_COMMITTER_DATE", "@150 +0000"),
    ];
    let g = |sp: &str, b: Option<&[u8]>| {
        let a: Vec<_> = sp.split('|').collect();
        run_git(root, &a, b, &env).unwrap()
    };
    assert!(g("init|-q", None).ok() && g("add|.", None).ok());
    let ci = "-c|user.name=t|-c|user.email=t@e|commit|-q|-m|Task: T-G";
    assert!(g(ci, None).ok());
    let att = br#"{"task":"T-G","merged_unix":250}"#;
    let oid = g("hash-object|-w|--stdin", Some(att));
    let hash = String::from_utf8_lossy(&oid.stdout);
    let uref = format!("update-ref|refs/merged/T-G|{}", hash.trim());
    assert!(g(&uref, None).ok());
    let out = run(root, &root.join("none"), &[], false, false).unwrap();
    assert!(out.contains("T-G"));
    let p = &score(root, None, None)["tasks"][0]["phases"];
    let pair = (p["implemented"].as_u64(), p["merged"].as_u64());
    assert_eq!(pair, (Some(150), Some(250)));
    let badf = root.join("e.jsonl");
    std::fs::write(&badf, "{bad\n").unwrap();
    assert!(run(root, &badf, &[], false, false).is_err());
}
