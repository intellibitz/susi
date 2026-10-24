#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The Harness cockpit plugin embeds the CEO cockpit, and the CEO cockpit is its
//! own standalone application — the former litellm/openclaw cockpit split out.
//! susi's own mission panel is a different surface, served by
//! `susi harness cockpit` on 9181.
//!
//! A plugin that keeps pointing at the old default embeds the wrong app, and
//! nothing else in the build would notice: these are JavaScript and JSON
//! assets. This is that guard.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(repo().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn the_plugin_embeds_the_standalone_ceo_cockpit() {
    let client = read("deploy/dsh-cockpit/client.js");
    assert!(
        client.contains("const DEFAULT_URL = 'http://localhost:8080/';"),
        "the cockpit panel must default to the standalone CEO cockpit on :8080"
    );
    assert!(
        !client.contains("DEFAULT_URL = 'http://127.0.0.1:9181"),
        "the old harness-panel URL must not be the plugin's default"
    );
}

/// A browser that saved the old default before the split must follow the new
/// one, or the panel keeps embedding the retired surface forever.
#[test]
fn a_saved_legacy_default_does_not_pin_the_old_cockpit() {
    let client = read("deploy/dsh-cockpit/client.js");
    assert!(
        client.contains("const LEGACY_DEFAULT = 'http://127.0.0.1:9181/?embed=1';"),
        "the previous default must be recognised so it can be ignored"
    );
    assert!(
        client.contains("if (stored && stored !== LEGACY_DEFAULT) return stored;"),
        "a stored legacy default must fall through to DEFAULT_URL"
    );
}

#[test]
fn the_plugin_describes_the_standalone_app_not_the_harness_panel() {
    let locale = read("deploy/dsh-cockpit/locale/en.json");
    assert!(
        locale.contains("localhost:8080"),
        "the offline hint must name the standalone cockpit's URL: {locale}"
    );
    assert!(
        !locale.contains("--port 9181"),
        "the offline hint must not tell the operator to start the harness panel: {locale}"
    );
    let faq = read("docs/harness-faq.md");
    assert!(
        faq.contains("http://localhost:8080/"),
        "the FAQ must name the standalone cockpit URL"
    );
}
