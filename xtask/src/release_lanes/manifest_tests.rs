fn rejected_manifest_lanes(lanes: Option<Value>) {
    let fx = Fx::new();
    let (work, _, _) = release_fixture(&fx);
    let git = Git { root: &work };
    let reference = release_ref("rc1");
    let tip = git.fetch_ref("origin", &reference).unwrap().unwrap();
    let mut manifest = read_manifest(&git, &tip, "rc1").unwrap();
    if let Some(lanes) = lanes {
        manifest["lanes"] = lanes;
    } else {
        manifest.as_object_mut().unwrap().remove("lanes");
    }
    let commit = git
        .commit_file(
            &tip,
            &manifest_path("rc1"),
            &json_bytes(&manifest).unwrap(),
            "incomplete release metadata",
        )
        .unwrap();
    assert!(matches!(
        git.push("origin", &[format!("{commit}:{reference}")], false)
            .unwrap(),
        Pushed::Done
    ));
    for args in [
        vec!["status", "--name", "rc1"],
        vec!["status", "--name", "rc1", "--json"],
    ] {
        let error = run_in(&work, &args).unwrap_err();
        assert!(error.contains("lane"), "{error}");
    }
}

#[test]
fn status_rejects_missing_empty_or_nonarray_lanes() {
    for lanes in [
        None,
        Some(json!([])),
        Some(Value::Null),
        Some(json!({})),
        Some(json!(true)),
    ] {
        rejected_manifest_lanes(lanes);
    }
}

#[test]
fn status_rejects_malformed_or_duplicate_lanes() {
    for lanes in [
        json!([null]),
        json!([{"status":"qualified"}]),
        json!([{"id":"lane-01"}]),
        json!([{"id":"lane-01","status":true}]),
        json!([{"id":"lane-01","status":"unknown"}]),
        json!([
            {"id":"lane-01","status":"qualified"},
            {"id":"lane-01","status":"qualified"}
        ]),
    ] {
        rejected_manifest_lanes(Some(lanes));
    }
}
