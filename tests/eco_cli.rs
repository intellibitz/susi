#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi ecosystem kb` end to end through the real binary: list, show,
//! search (kind/capability/vendor filters) and check over a store dir.

use std::path::{Path, PathBuf};
use std::process::Command;

fn susi(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env_remove("SUSI_HOME")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const PROV: &str = r#""provenance":{"source":"https://specs.example.test/v1","spec_version":"1.0","retrieved":"2026-09-01","confidence":"verified"}"#;

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
}

/// A small consistent store: vendor provides engine; engine has a
/// capability and implements a protocol version.
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
    let root = std::env::temp_dir().join(format!("susi-eco-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    root
}

#[test]
fn kb_list_show_search_and_check_through_the_binary() {
    let root = root("main");
    let home = root.join("home");
    let store = fixture_store(&root);
    let store_arg = store.to_str().unwrap().to_string();

    let (code, out, err) = susi(&home, &["ecosystem", "kb", "list", "--dir", &store_arg]);
    assert_eq!(code, 0, "{err}");
    let list: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["id"].as_str())
        .collect();
    for id in [
        "acme",
        "acme-engine",
        "cap-tool-calling",
        "proto",
        "proto-v1",
    ] {
        assert!(ids.contains(&id), "{id} missing from {out}");
    }

    let (code, out, err) = susi(
        &home,
        &[
            "ecosystem",
            "kb",
            "show",
            "acme-engine",
            "--dir",
            &store_arg,
        ],
    );
    assert_eq!(code, 0, "{err}");
    let show: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(show["entity"]["id"], "acme-engine");
    assert_eq!(show["outgoing"].as_array().unwrap().len(), 2);
    assert_eq!(show["incoming"].as_array().unwrap().len(), 1);

    // Search: text query.
    let (code, out, err) = susi(
        &home,
        &["ecosystem", "kb", "search", "acme", "--dir", &store_arg],
    );
    assert_eq!(code, 0, "{err}");
    let search: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(search["count"].as_u64().unwrap(), 2, "{out}");

    // Search: kind filter.
    let (code, out, err) = susi(
        &home,
        &[
            "ecosystem",
            "kb",
            "search",
            "--kind",
            "vendor",
            "--dir",
            &store_arg,
        ],
    );
    assert_eq!(code, 0, "{err}");
    let search: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(search["count"].as_u64().unwrap(), 1, "{out}");

    // Search: capability filter via a vendor spelling ("function calling").
    let (code, out, err) = susi(
        &home,
        &[
            "ecosystem",
            "kb",
            "search",
            "--capability",
            "function calling",
            "--dir",
            &store_arg,
        ],
    );
    assert_eq!(code, 0, "{err}");
    let search: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(search["count"].as_u64().unwrap(), 1, "{out}");
    assert_eq!(search["entities"][0]["id"], "acme-engine");

    // Search: vendor filter returns vendor + provided component.
    let (code, out, err) = susi(
        &home,
        &[
            "ecosystem",
            "kb",
            "search",
            "--vendor",
            "acme",
            "--dir",
            &store_arg,
        ],
    );
    assert_eq!(code, 0, "{err}");
    let search: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(search["count"].as_u64().unwrap(), 2, "{out}");

    // Check on a consistent store passes.
    let (code, out, err) = susi(&home, &["ecosystem", "kb", "check", "--dir", &store_arg]);
    assert_eq!(code, 0, "{err}\n{out}");
    let check: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(check["issues"].as_array().unwrap().len(), 0, "{out}");

    // Unknown id exits nonzero.
    let (code, _, err) = susi(
        &home,
        &["ecosystem", "kb", "show", "ghost", "--dir", &store_arg],
    );
    assert_ne!(code, 0);
    assert!(err.contains("ghost"), "{err}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn kb_check_fails_on_a_broken_store() {
    let root = root("broken");
    let home = root.join("home");
    let store = fixture_store(&root);
    let bad = root.join("broken");
    std::fs::create_dir_all(&bad).unwrap();
    for entry in std::fs::read_dir(&store).unwrap() {
        let p = entry.unwrap().path();
        std::fs::copy(&p, bad.join(p.file_name().unwrap())).unwrap();
    }
    // Dangling relation target.
    let dangling = format!(
        r#"{{"version":"eco-schema/v1","entity":{{"kind":"component","id":"needs-ghost","name":"Needs Ghost","category":"cli-tool",{PROV}}},"relations":[{{"kind":"depends-on","from":"needs-ghost","to":"ghost",{PROV}}}]}}"#
    );
    std::fs::write(bad.join("needs-ghost.json"), dangling).unwrap();

    let (code, out, _) = susi(
        &home,
        &["ecosystem", "kb", "check", "--dir", bad.to_str().unwrap()],
    );
    assert_ne!(code, 0, "{out}");
    let check: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(!check["issues"].as_array().unwrap().is_empty());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn kb_defaults_to_the_layered_store() {
    let root = root("empty");
    let home = root.join("home");
    // Nothing seeded: layered store is empty but the command still works.
    let (code, out, err) = susi(&home, &["ecosystem", "kb", "list"]);
    assert_eq!(code, 0, "{err}");
    let list: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);
    std::fs::remove_dir_all(&root).ok();
}
