#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! T-CLAUDE-348 — `susi ecosystem explain <thing>` through the real binary.

use std::path::{Path, PathBuf};
use std::process::Command;
use susi_paths::test_env::isolate_home;

fn susi(home: &Path, args: &[&str]) -> (i32, String, String) {
    susi_with(home, args, &[])
}

fn susi_with(home: &Path, args: &[&str], overrides: &[(&str, &Path)]) -> (i32, String, String) {
    std::fs::create_dir_all(home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    let cmd = isolate_home(&mut cmd, home);
    cmd.current_dir(home)
        .args(args)
        .env("SUSI_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("XDG_DATA_HOME", home.join("xdg-data"));
    for key in ["SUSI_ECO_BUNDLED", "SUSI_ECO_SOURCE", "SUSI_ECO_PROFILES"] {
        cmd.env_remove(key);
    }
    let out = cmd.envs(overrides.iter().copied()).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn profile(dir: &Path, id: &str, subject: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(include_str!(
        "../config/ecosystem/profiles/openai-chat-completions.json"
    ))
    .unwrap();
    doc["id"] = id.into();
    doc["subject"] = subject.into();
    write(dir, id, &doc.to_string());
}

fn subject(
    home: &Path,
    id: &str,
    root: Option<&Path>,
    overrides: &[(&str, &Path)],
) -> serde_json::Value {
    let mut args = vec!["ecosystem", "explain", id];
    if let Some(dir) = root {
        args.extend(["--dir", dir.to_str().unwrap()]);
    }
    let (code, out, err) = susi_with(home, &args, overrides);
    assert_eq!(code, 0, "{err}");
    serde_json::from_str::<serde_json::Value>(&out).unwrap()["susi_profile"].clone()
}

#[test]
fn fresh_install_finds_packaged_profiles_and_honors_whole_user_overrides() {
    let root = root("packaged");
    let home = &root.join("home");
    let id = "openai-chat-completions";
    assert_eq!(subject(home, id, None, &[]), id);
    let user = home.join("state/ecosystem/profiles");
    profile(&user, id, "other-subject");
    assert!(subject(home, id, None, &[]).is_null());
    profile(&user, "user-profile", id);
    assert_eq!(subject(home, id, None, &[]), "user-profile");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn selected_store_controls_profiles_and_reports_corrupt_files() {
    let root = root("selected");
    let home = root.join("home");
    let store = fixture_store(&root);
    let other = root.join("other-profiles");
    profile(&other, "overlay", "proto");
    let overrides = [
        ("SUSI_ECO_PROFILES", other.as_path()),
        ("SUSI_ECO_BUNDLED", store.as_path()),
    ];
    assert_eq!(subject(&home, "proto", None, &overrides), "overlay");
    assert!(subject(&home, "proto", Some(&store), &overrides).is_null());
    let profiles = store.join("profiles");
    profile(&profiles, "from-selected-store", "proto");
    assert_eq!(
        subject(&home, "proto", Some(&store), &overrides),
        "from-selected-store"
    );
    assert_eq!(
        subject(
            &home,
            "proto",
            None,
            &[("SUSI_ECO_SOURCE", store.as_path())]
        ),
        "from-selected-store"
    );
    write(&profiles, "broken", "{");
    let (code, _, err) = susi_with(
        &home,
        &[
            "ecosystem",
            "explain",
            "proto",
            "--dir",
            store.to_str().unwrap(),
        ],
        &overrides,
    );
    assert_ne!(code, 0);
    assert!(err.contains("broken.json"), "{err}");
    std::fs::remove_dir_all(root).unwrap();
}

const PROV: &str = r#""provenance":{"source":"https://specs.example.test/v1","spec_version":"1.0","retrieved":"2026-09-01","confidence":"verified"}"#;

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
}

/// vendor -> engine -> capability, engine implements proto-v1 (version-of proto).
fn fixture_store(root: &Path) -> PathBuf {
    let dir = root.join("store");
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "acme",
        &format!(
            r#"{{"version":"eco-schema/v1","entity":{{"kind":"vendor","id":"acme","name":"Acme","home":"https://acme.example.test",{PROV}}},"relations":[{{"kind":"provides","from":"acme","to":"acme-engine",{PROV}}}]}}"#
        ),
    );
    write(
        &dir,
        "acme-engine",
        &format!(
            r#"{{"version":"eco-schema/v1","entity":{{"kind":"component","id":"acme-engine","name":"Acme Engine","category":"inference-engine",{PROV}}},"relations":[{{"kind":"has-capability","from":"acme-engine","to":"cap-tool-calling",{PROV}}},{{"kind":"implements-version","from":"acme-engine","to":"proto-v1",{PROV}}}]}}"#
        ),
    );
    write(
        &dir,
        "cap-tool-calling",
        &format!(
            r#"{{"version":"eco-schema/v1","entity":{{"kind":"capability","id":"cap-tool-calling","name":"Tool calling","class":"tool-calling",{PROV}}},"relations":[]}}"#
        ),
    );
    write(
        &dir,
        "proto",
        &format!(
            r#"{{"version":"eco-schema/v1","entity":{{"kind":"protocol","id":"proto","name":"Proto","transports":["http"],{PROV}}},"relations":[]}}"#
        ),
    );
    write(
        &dir,
        "proto-v1",
        &format!(
            r#"{{"version":"eco-schema/v1","entity":{{"kind":"spec-version","id":"proto-v1","name":"v1","version":"1","released":null,{PROV}}},"relations":[{{"kind":"version-of","from":"proto-v1","to":"proto",{PROV}}}]}}"#
        ),
    );
    dir
}

fn root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("susi-eco-explain-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    root
}

#[test]
fn eco_explain_a_protocol_shows_versions_and_implementers() {
    let root = root("proto");
    let home = root.join("home");
    let store = fixture_store(&root);
    let store_arg = store.to_str().unwrap().to_string();

    let (code, out, err) = susi(
        &home,
        &["ecosystem", "explain", "proto", "--dir", &store_arg],
    );
    assert_eq!(code, 0, "{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["entity"]["id"], "proto");
    assert_eq!(doc["entity"]["kind"], "protocol");
    // what versions exist of it
    assert!(doc["versions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "proto-v1"));
    // who implements it
    assert!(doc["implemented_by"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "acme-engine"));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn eco_explain_a_component_shows_provider_and_capabilities() {
    let root = root("engine");
    let home = root.join("home");
    let store = fixture_store(&root);
    let store_arg = store.to_str().unwrap().to_string();

    let (code, out, err) = susi(
        &home,
        &["ecosystem", "explain", "acme-engine", "--dir", &store_arg],
    );
    assert_eq!(code, 0, "{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["entity"]["id"], "acme-engine");
    assert!(doc["provided_by"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "acme"));
    assert!(doc["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "cap-tool-calling"));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn eco_explain_unknown_id_exits_nonzero_with_hint() {
    let root = root("ghost");
    let home = root.join("home");
    let store = fixture_store(&root);
    let store_arg = store.to_str().unwrap().to_string();

    let (code, _out, err) = susi(
        &home,
        &["ecosystem", "explain", "ghost", "--dir", &store_arg],
    );
    assert_ne!(code, 0);
    assert!(err.contains("ghost"), "{err}");

    std::fs::remove_dir_all(&root).ok();
}
