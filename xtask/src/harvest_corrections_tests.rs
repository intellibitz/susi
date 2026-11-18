fn correction_fixture() -> (Fixture, Value, String) {
    let fx = Fixture::new();
    let sha = fx.intake();
    fx.import_widgets(&sha).unwrap();
    fx.git_at(&fx.work, &["add", "-A"]);
    fx.git_at(
        &fx.work,
        &[
            "commit",
            "-q",
            "-m",
            "import widgets\n\nHarvest: original-apache-2.0\nTask: T-TEST-1",
        ],
    );
    let commit = fx.git_at(&fx.work, &["rev-parse", "HEAD"]);
    let git = Git {
        root: fx.work.clone(),
    };
    let tree = list_tree(&git, &commit).unwrap();
    let manifest: Value = serde_json::from_str(&fx.read(".agents/harvest/H-TEST-1.json")).unwrap();
    let files: Vec<_> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            let path = file["dest_path"].as_str().unwrap();
            json!({"path":path,"commit_blob_sha1":tree[path].oid})
        })
        .collect();
    let record = json!({"schema":"susi-harvest-trailer-corrections-v1","corrections":[{
        "commit":commit,"old_label":"original-apache-2.0","manifest_id":"H-TEST-1",
        "files":files,"reason":"The original immutable commit used a prose label instead of the actual manifest ID."
    }]});
    (fx, record, commit)
}

fn write_correction(fx: &Fixture, record: &Value) {
    write(
        &fx.work,
        ".agents/harvest-corrections.json",
        &record.to_string(),
    );
}

#[test]
fn audit_correction_refuses_duplicate_json_keys_including_escaped_spellings() {
    let (fx, record, commit) = correction_fixture();
    for name in ["commit", "\\u0063ommit"] {
        let raw = record.to_string().replacen(
            "\"commit\":",
            &format!("\"{name}\":\"{commit}\",\"commit\":"),
            1,
        );
        write(&fx.work, ".agents/harvest-corrections.json", &raw);
        assert!(fx
            .run(&["audit", "--range", &commit])
            .unwrap_err()
            .contains("duplicate"));
    }
    let mut good = record.clone();
    good["corrections"][0]["reason"] = "Quoted text \\\"commit\\\": and braces {} are data.".into();
    write_correction(&fx, &good);
    assert!(fx.run(&["audit", "--range", &commit]).is_ok());
}

#[test]
fn audit_correction_repairs_only_the_bound_history_and_keeps_current_source_checks() {
    let (fx, record, commit) = correction_fixture();
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("original-apache-2.0"));
    write_correction(&fx, &record);
    let passed = fx.run(&["audit", "--range", &commit]).unwrap();
    assert!(
        passed.contains("1 manifest(s), 2 file(s), 1 Harvest-trailer commit(s)"),
        "{passed}"
    );
    write(
        &fx.work,
        "vendor/widgets/widget.rs",
        "pub fn changed_without_attribution() {}\n",
    );
    let error = fx.run(&["audit", "--range", &commit]).unwrap_err();
    assert!(
        error.contains("Modified by") && error.contains("copyright"),
        "{error}"
    );
}

#[test]
fn audit_correction_refuses_wrong_commits_labels_manifests_and_unbound_fields() {
    let (fx, record, commit) = correction_fixture();
    for (field, value) in [
        ("commit", "f".repeat(40)),
        ("commit", fx.git_at(&fx.work, &["rev-parse", "HEAD^"])),
        ("old_label", "different-label".to_owned()),
        ("old_label", "H-TEST-1".to_owned()),
        ("manifest_id", "H-TEST-99".to_owned()),
        ("reason", " ".to_owned()),
    ] {
        let mut bad = record.clone();
        bad["corrections"][0][field] = value.into();
        write_correction(&fx, &bad);
        assert!(fx.run(&["audit", "--range", &commit]).is_err(), "{field}");
    }
    for level in [0, 1, 2] {
        let mut bad = record.clone();
        let value = match level {
            0 => &mut bad,
            1 => &mut bad["corrections"][0],
            _ => &mut bad["corrections"][0]["files"][0],
        };
        value["unbound"] = "ignored evidence must refuse".into();
        write_correction(&fx, &bad);
        assert!(fx
            .run(&["audit", "--range", &commit])
            .unwrap_err()
            .contains("unbound"));
    }
}

#[test]
fn audit_correction_refuses_duplicate_records_paths_wrong_blobs_and_partial_coverage() {
    let (fx, record, commit) = correction_fixture();
    let mut duplicate = record.clone();
    duplicate["corrections"]
        .as_array_mut()
        .unwrap()
        .push(record["corrections"][0].clone());
    write_correction(&fx, &duplicate);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("duplicate"));
    let mut duplicate = record.clone();
    duplicate["corrections"][0]["files"]
        .as_array_mut()
        .unwrap()
        .push(record["corrections"][0]["files"][0].clone());
    write_correction(&fx, &duplicate);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("duplicated"));
    let mut bad = record.clone();
    bad["corrections"][0]["files"][0]["commit_blob_sha1"] = "0".repeat(40).into();
    write_correction(&fx, &bad);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("source blob"));
    let mut partial = record.clone();
    partial["corrections"][0]["files"]
        .as_array_mut()
        .unwrap()
        .pop();
    write_correction(&fx, &partial);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("exactly all imported"));
}

#[test]
fn audit_correction_refuses_an_unrelated_valid_provenance_manifest() {
    let (fx, mut record, commit) = correction_fixture();
    let mut unrelated: Value =
        serde_json::from_str(&fx.read(".agents/harvest/H-TEST-1.json")).unwrap();
    unrelated["id"] = "H-TEST-2".into();
    for file in unrelated["files"].as_array_mut().unwrap() {
        let old = file["dest_path"].as_str().unwrap();
        let new = old.replace("vendor/widgets/", "vendor/unrelated/");
        write(&fx.work, &new, &fx.read(old));
        file["dest_path"] = new.into();
    }
    write(
        &fx.work,
        ".agents/harvest/H-TEST-2.json",
        &unrelated.to_string(),
    );
    fx.run(&["notices"]).unwrap();
    assert!(fx.run(&["audit"]).is_ok());
    record["corrections"][0]["manifest_id"] = "H-TEST-2".into();
    write_correction(&fx, &record);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("does not cover imported"));
}

#[test]
fn audit_correction_requires_ancestry_and_never_aliases_another_commit() {
    let (fx, record, commit) = correction_fixture();
    let tree = fx.git_at(&fx.work, &["rev-parse", "HEAD^{tree}"]);
    let foreign = fx.git_at(
        &fx.work,
        &[
            "commit-tree",
            &tree,
            "-m",
            "unrelated\n\nHarvest: original-apache-2.0",
        ],
    );
    let mut bad = record.clone();
    bad["corrections"][0]["commit"] = foreign.into();
    write_correction(&fx, &bad);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("history"));
    write_correction(&fx, &record);
    write(&fx.work, "README.md", "later unrelated change\n");
    fx.git_at(&fx.work, &["add", "README.md"]);
    fx.git_at(
        &fx.work,
        &[
            "commit",
            "-q",
            "-m",
            "another bad label\n\nHarvest: original-apache-2.0",
        ],
    );
    assert!(fx
        .run(&["audit", "--range", "HEAD"])
        .unwrap_err()
        .contains("original-apache-2.0"));
}

/// A commit that moved first-party code within this repository under a prose
/// Harvest label, its strictly earlier source commit, and the bound record.
fn repository_fixture() -> (Fixture, Value, String, String) {
    let fx = Fixture::new();
    write(&fx.work, "src/original.rs", "pub fn original() {}\n");
    fx.git_at(&fx.work, &["add", "src/original.rs"]);
    fx.git_at(&fx.work, &["commit", "-q", "-m", "first-party original"]);
    let source = fx.git_at(&fx.work, &["rev-parse", "HEAD"]);
    write(&fx.work, "src/moved.rs", "pub fn moved() {}\n");
    fx.git_at(&fx.work, &["add", "src/moved.rs"]);
    fx.git_at(
        &fx.work,
        &[
            "commit",
            "-q",
            "-m",
            "move first-party code\n\nHarvest: original-apache-2.0",
        ],
    );
    let commit = fx.git_at(&fx.work, &["rev-parse", "HEAD"]);
    let git = Git {
        root: fx.work.clone(),
    };
    let (moved, original) = (
        list_tree(&git, &commit).unwrap(),
        list_tree(&git, &source).unwrap(),
    );
    let record = json!({"schema":"susi-harvest-trailer-corrections-v1","corrections":[{
        "commit":commit,"old_label":"original-apache-2.0","source_commit":source,
        "files":[{"path":"src/moved.rs","commit_blob_sha1":moved["src/moved.rs"].oid,
            "source_path":"src/original.rs","source_blob_sha1":original["src/original.rs"].oid}],
        "reason":"First-party code moved within this repository; nothing was imported."
    }]});
    (fx, record, commit, source)
}

#[test]
fn audit_correction_accepts_first_party_code_moved_from_an_earlier_commit() {
    let (fx, record, commit, _) = repository_fixture();
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("original-apache-2.0"));
    write_correction(&fx, &record);
    assert!(fx.run(&["audit", "--range", &commit]).is_ok());
}

#[test]
fn audit_correction_refuses_unproven_or_ambiguous_repository_sources() {
    let (fx, record, commit, source) = repository_fixture();
    let tree = fx.git_at(&fx.work, &["rev-parse", "HEAD^{tree}"]);
    let foreign = fx.git_at(&fx.work, &["commit-tree", &tree, "-m", "unrelated"]);
    for (field, value) in [
        ("source_commit", commit.clone()),
        ("source_commit", foreign),
        ("source_commit", "f".repeat(40)),
    ] {
        let mut bad = record.clone();
        bad["corrections"][0][field] = value.into();
        write_correction(&fx, &bad);
        assert!(fx
            .run(&["audit", "--range", &commit])
            .unwrap_err()
            .contains("strictly earlier"));
    }
    for (field, value) in [
        ("source_path", "src/absent.rs".to_owned()),
        ("source_path", "../src/original.rs".to_owned()),
        ("source_blob_sha1", "0".repeat(40)),
    ] {
        let mut bad = record.clone();
        bad["corrections"][0]["files"][0][field] = value.into();
        write_correction(&fx, &bad);
        assert!(fx
            .run(&["audit", "--range", &commit])
            .unwrap_err()
            .contains("differs from the earlier commit"));
    }
    let mut both = record.clone();
    both["corrections"][0]["manifest_id"] = "H-TEST-1".into();
    write_correction(&fx, &both);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("unbound"));
    let mut bare = record.clone();
    let file = bare["corrections"][0]["files"][0].as_object_mut().unwrap();
    file.remove("source_path");
    file.remove("source_blob_sha1");
    write_correction(&fx, &bare);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("unbound"));
    write(&fx.work, "src/later.rs", "pub fn later() {}\n");
    fx.git_at(&fx.work, &["add", "src/later.rs"]);
    fx.git_at(&fx.work, &["commit", "-q", "-m", "later"]);
    let later = fx.git_at(&fx.work, &["rev-parse", "HEAD"]);
    let mut descendant = record.clone();
    descendant["corrections"][0]["source_commit"] = later.into();
    write_correction(&fx, &descendant);
    assert!(fx
        .run(&["audit", "--range", &commit])
        .unwrap_err()
        .contains("strictly earlier"));
    assert_ne!(source, commit);
}
