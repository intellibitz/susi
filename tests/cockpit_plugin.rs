#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The Harness cockpit plugin registers **two** cockpits, and they are not
//! interchangeable:
//!
//! - the **external** one embeds the standalone orchestration cockpit app at
//!   `http://localhost:8080/` (the former litellm/openclaw cockpit, now its own
//!   application);
//! - the **internal** one embeds susi's own panel, built in-house and served by
//!   `susi harness cockpit`, at `http://127.0.0.1:9181/?embed=1`.
//!
//! A plugin that points them at each other, or that loses one of them, embeds
//! the wrong surface — and nothing else in the build would notice, because
//! these are JavaScript and JSON assets. This is that guard.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(repo().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn the_plugin_registers_both_cockpits_as_sidebar_entries_and_panels() {
    let client = read("deploy/dsh-cockpit/client.js");
    for id in ["susi-cockpit-external", "susi-cockpit-internal"] {
        assert!(
            client.contains(&format!("id: '{id}'")),
            "{id} must be a cockpit entry"
        );
    }
    // The sidebar button and the main panel must be bound to the same key, or a
    // button opens someone else's panel.
    let sidebar = client
        .find("name: 'sidebar.panellist'")
        .expect("sidebar registration");
    let panel = client.find("name: 'main'").expect("main registration");
    assert!(
        client[sidebar..panel].contains("id: cockpit.id"),
        "the sidebar entry must use the cockpit's own id"
    );
    assert!(
        client[panel..].contains("key: cockpit.id"),
        "the main panel must use the same id as its sidebar entry"
    );
    assert!(
        client.contains("for (const [index, cockpit] of COCKPITS.entries())"),
        "both cockpits must be registered from the same loop"
    );
}

/// The external cockpit embeds the standalone app; the internal one embeds
/// susi's own panel. Swapping them is the failure this guards.
#[test]
fn each_cockpit_embeds_its_own_surface() {
    let client = read("deploy/dsh-cockpit/client.js");
    let external = client
        .find("id: 'susi-cockpit-external'")
        .expect("external cockpit entry");
    let internal = client
        .find("id: 'susi-cockpit-internal'")
        .expect("internal cockpit entry");
    let external_block = &client[external..internal];
    let internal_block = &client[internal..];

    assert!(
        external_block.contains("defaultUrl: 'http://localhost:8080/'"),
        "the external cockpit must default to the standalone app on :8080"
    );
    assert!(
        internal_block.contains("defaultUrl: 'http://127.0.0.1:9181/?embed=1'"),
        "the internal cockpit must default to susi's own panel on :9181"
    );
    assert!(
        external_block.contains("susi.cockpit.external.url")
            && internal_block.contains("susi.cockpit.internal.url"),
        "each cockpit must remember its own URL, or repointing one moves the other"
    );
}

/// Before there were two, `susi.cockpit.url` held the native panel's URL. A URL
/// an operator saved there must be adopted by that panel, never by the external
/// app and never lost.
#[test]
fn the_native_cockpit_adopts_the_legacy_single_cockpit_url() {
    let client = read("deploy/dsh-cockpit/client.js");
    let internal = client
        .find("id: 'susi-cockpit-internal'")
        .expect("internal cockpit entry");
    assert!(
        client[internal..].contains("legacyKey: 'susi.cockpit.url'"),
        "the native cockpit must adopt the pre-split URL key"
    );
    let external = client
        .find("id: 'susi-cockpit-external'")
        .expect("external cockpit entry");
    assert!(
        client[external..internal].contains("legacyKey: null"),
        "the external cockpit must not inherit the native panel's saved URL"
    );
}

#[test]
fn the_plugin_describes_both_cockpits() {
    let locale = read("deploy/dsh-cockpit/locale/en.json");
    assert!(
        locale.contains("localhost:8080") && locale.contains("--port 9181"),
        "the offline hint must name both cockpits' ways to start: {locale}"
    );
    let docs = read("docs/harness.md");
    for needle in ["CEO cockpit (external)", "CEO cockpit (susi native)"] {
        assert!(
            docs.contains(needle),
            "harness.md must name the {needle} entry"
        );
    }
}
