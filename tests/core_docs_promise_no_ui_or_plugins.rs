//! Core documentation must not promise what core no longer is (BT-06).
//!
//! CEO directive 2026-11-03: the free Apache-2.0 core is a pure CLI plus
//! standards socket. It ships no UI, bundles no plugin content, starts and
//! downloads no third-party software on its own, and is released only for
//! x86_64 Windows and Linux (the source stays portable). Every UI and every
//! curated plugin lives in Susi Commercial Apps.
//!
//! This test reads `README.md`, `ARCHITECTURE.md`, `VISION.md` and every
//! Markdown file under `docs/`, splits them into sentences (table rows, list
//! items, headings and code lines are their own units) and fails on any
//! sentence that makes one of the banned promises below.
//!
//! Markdown emphasis and code ticks are stripped before matching, so bold
//! text cannot split a banned phrase. Short roots (`gui`, `dashboard`,
//! `cockpit`, `macos`) match as whole words in prose only, outside code spans
//! and link targets; `macos` fires only next to a release word.
//!
//! Three narrow exemptions, all explicit:
//! - a sentence that says the UI or plugins it names live in (or moved to)
//!   Susi Commercial Apps or the commercial desktop ([`RELOCATION`]); a bare
//!   mention of the suite is not enough;
//! - a sentence that tags itself as history: it opens with `Retracted` or
//!   `Known gap`, or carries a `(historical` / `(superseded` note;
//! - and [`ALLOWED`], a short (file, fragment) list for historical evidence
//!   logs and the few sentences whose banned word is not a core promise.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use std::path::{Path, PathBuf};

/// One banned promise: a lowercase fragment and the rule it breaks.
struct Ban {
    fragment: &'static str,
    rule: &'static str,
    /// Match only as a whole word (an optional plural `s` allowed) in the
    /// sentence's prose, outside inline code spans and link targets, so a
    /// short root such as `gui` or `cockpit` never hits `guide` or the
    /// `cockpit` module name.
    word: bool,
    /// When non-empty, the ban fires only if the sentence also names one of
    /// these (a platform name is fine; a platform *release* is not).
    needs: &'static [&'static str],
}

/// A plain substring ban.
const fn ban(fragment: &'static str, rule: &'static str) -> Ban {
    Ban {
        fragment,
        rule,
        word: false,
        needs: &[],
    }
}

/// A whole-word ban on the sentence's prose.
const fn word(fragment: &'static str, rule: &'static str) -> Ban {
    Ban {
        fragment,
        rule,
        word: true,
        needs: &[],
    }
}

/// Words that turn a platform name into a release promise (whole words in
/// prose; a plural `s` is implied).
const RELEASE_WORDS: &[&str] = &[
    "release",
    "released",
    "ship",
    "shipped",
    "publish",
    "publishes",
    "published",
    "download",
    "downloaded",
    "binary",
    "binaries",
    "asset",
    "build",
    "built",
];

const UI: &str = "core ships no UI (it lives in Susi Commercial Apps)";
const PLUGINS: &str = "core bundles no plugin content (it lives in Susi Commercial Apps)";
const AUTO: &str = "core starts, enables and downloads no third-party software on its own";
const PLATFORM: &str = "core releases only for x86_64 Windows and Linux";

const BANS: &[Ban] = &[
    // A core UI: the retired HTML cockpit, the harness desktop plugin, the
    // full-screen redraw and the interactive shell prompt. `cockpit` itself
    // is a whole-word ban below, so the `cockpit` module name stays legal.
    ban("panel.html", UI),
    ban("html panel", UI),
    ban("harness-cockpit", UI),
    ban("dsh-cockpit", UI),
    ban("web ui", UI),
    ban("web dashboard", UI),
    ban("html dashboard", UI),
    ban("browser ui", UI),
    ban("desktop ui", UI),
    ban("graphical interface", UI),
    ban("terminal ui", UI),
    ban("tui dashboard", UI),
    ban("top --watch", UI),
    ban("full-screen", UI),
    ban("susi shell", UI),
    ban("susi> ", UI),
    ban("interactive shell", UI),
    ban("shell prompt", UI),
    ban("secure interactive ui", UI),
    ban("full host ux", UI),
    word("gui", UI),
    word("dashboard", UI),
    word("cockpit", UI),
    // Bundled plugin content: a seeded default pack, the deleted vendor
    // catalogs and the removed harness plugin registry.
    ban("default pack", PLUGINS),
    ban("default extension pack", PLUGINS),
    ban("bundled catalog", PLUGINS),
    ban("bundled pack", PLUGINS),
    ban("extensions seed", PLUGINS),
    ban("seeds the default", PLUGINS),
    ban("leading servers (top", PLUGINS),
    ban("scout packages", PLUGINS),
    ban("curated leading servers", PLUGINS),
    ban("plugins.rs", PLUGINS),
    ban("harness.plugins.json", PLUGINS),
    ban("plugins.example.json", PLUGINS),
    ban("plugin triad", PLUGINS),
    ban("plugin lifecycle", PLUGINS),
    ban("pluginmanager", PLUGINS),
    ban("pluginreloader", PLUGINS),
    ban("list_plugins", PLUGINS),
    ban("bundled plugin", PLUGINS),
    ban("bundles plugin", PLUGINS),
    ban("ships plugins", PLUGINS),
    // Automatic third-party launches and downloads.
    ban("auto-enable", AUTO),
    ban("auto-link", AUTO),
    ban("auto-launch", AUTO),
    ban("autolaunch", AUTO),
    ban("launches automatically", AUTO),
    ban("auto-primes packs", AUTO),
    ban("primes every catalog at boot", AUTO),
    ban("auto-download", AUTO),
    ban("automatic download", AUTO),
    ban("downloads automatically", AUTO),
    ban("automatically download", AUTO),
    ban("downloads the starter", AUTO),
    ban("auto-start", AUTO),
    ban("autostart", AUTO),
    ban("starts automatically", AUTO),
    ban("automatically start", AUTO),
    ban("auto-install", AUTO),
    ban("installs automatically", AUTO),
    ban("automatically install", AUTO),
    ban("automatic starter download", AUTO),
    ban("provisions the licensed starter", AUTO),
    ban("starts a background download", AUTO),
    ban("version-probe only", AUTO),
    ban("version-probe refreshed", AUTO),
    ban("tick = probe_all", AUTO),
    // macOS / ARM core releases.
    ban("macos release", PLATFORM),
    ban("macos binary", PLATFORM),
    ban("macos binaries", PLATFORM),
    ban("macos asset", PLATFORM),
    ban("macos engine artifact", PLATFORM),
    ban("macos-aarch64", PLATFORM),
    ban("macos-x86_64", PLATFORM),
    ban("susi-macos", PLATFORM),
    ban("apple-darwin", PLATFORM),
    ban("linux-aarch64", PLATFORM),
    ban("windows-aarch64", PLATFORM),
    ban("arm64 release", PLATFORM),
    ban("arm64 binary", PLATFORM),
    ban("arm64 asset", PLATFORM),
    ban("publish-extra", PLATFORM),
    ban("aarch64-unknown-linux", PLATFORM),
    ban("aarch64-pc-windows", PLATFORM),
    ban("x86_64-apple", PLATFORM),
    ban("linux arm64 finished", PLATFORM),
    ban("optional-platform matrix", PLATFORM),
    ban("release's slowest legs are macos", PLATFORM),
    Ban {
        fragment: "macos",
        rule: PLATFORM,
        word: true,
        needs: RELEASE_WORDS,
    },
];

/// A sentence saying the UI or plugins it names moved to (or live in) the
/// commercial suite. A bare mention of the suite is not enough.
const RELOCATION: &[&str] = &[
    "lives in susi commercial apps",
    "live in susi commercial apps",
    "lives in the commercial desktop",
    "live in the commercial desktop",
    "moved to susi commercial apps",
    "move to susi commercial apps",
    "moves to susi commercial apps",
];

/// A sentence (list item, table row, quote or heading) that opens with one of
/// these tags is history, not a promise.
const HISTORY_TAGS: &[&str] = &["retracted", "known gap"];

/// A sentence carrying one of these parenthetical tags is history.
const HISTORY_NOTES: &[&str] = &["(historical", "(superseded"];

/// Historical evidence logs: (file, sentence fragment) pairs that record a
/// past measurement or the past release pipeline and carry no marker.
const ALLOWED: &[(&str, &str)] = &[
    // Release legs measured on v0.21.27 (2026-10-31), before the CEO
    // decision of 2026-11-03 cut the macOS/ARM legs from core.
    (
        "docs/workflow-speed.md",
        "| build macos-aarch64 | cancelled after 1 s",
    ),
    // v0.21.29 timing log of the former publish-extra job.
    (
        "docs/workflow-speed.md",
        "arm64, macos and cuda upload when they finish",
    ),
    // Mandate 62 plugin-update evidence recorded with the since-removed
    // `extensions seed`; plan item E2 re-records it with a commercial pack.
    (
        "docs/workflow-speed.md",
        "delivered to a running binary as an extension pack (extensions seed",
    ),
    // The same release-leg table: the Linux ARM64 build of v0.21.27.
    ("docs/workflow-speed.md", "| build linux-x86_64 / aarch64 |"),
    // Step 1 release-D evidence of the starter download (2026-11-02).
    (
        "docs/step1-acceptance.md",
        "the automatic starter download reached100%",
    ),
    // ARM64 Windows portable-source note: the target is named only to say it
    // is not a published core release and qualifies downstream.
    (
        "docs/step1-acceptance.md",
        "arm64 windows (aarch64-pc-windows-msvc) portable source support exists",
    ),
    // The client-agnostic GMCP contract: "a dashboard" is any reader.
    (
        "docs/gmcp/contract-203-001.md",
        "so a dashboard or operator sees one canonical shape",
    ),
    // The `auto-install` helper module of `susi-sandbox-client`.
    (
        "ARCHITECTURE.md",
        "audit-chain / auto-install / daemon-state",
    ),
    // v0.21.26 and v0.21.29 release timings, before the macOS legs were cut.
    (
        "docs/workflow-speed.md",
        "| tag → release published (v0.21.26) | 92 min |",
    ),
    (
        "docs/workflow-speed.md",
        "on v0.21.29 the wall was windows at 24 min",
    ),
    // The provider's own billing page, not a core view.
    (
        "docs/harness-faq.md",
        "a provider's own dashboard cannot say which task spent the money",
    ),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn markdown_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            markdown_under(&path, out);
        } else if path.extension().is_some_and(|x| x == "md") {
            out.push(path);
        }
    }
}

/// The documents this test owns: the three front-door files and `docs/`.
fn scanned_docs() -> Vec<(String, String)> {
    let root = root();
    let mut paths: Vec<PathBuf> = ["README.md", "ARCHITECTURE.md", "VISION.md"]
        .iter()
        .map(|rel| root.join(rel))
        .collect();
    markdown_under(&root.join("docs"), &mut paths);
    paths
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            (rel, text)
        })
        .collect()
}

/// Split a Markdown document into sentences. Prose paragraphs are joined and
/// split at sentence ends; table rows, list items, headings, quotes and code
/// lines each start a new unit so one row can never borrow another's marker.
fn sentences(text: &str) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    let flush = |current: &mut String, units: &mut Vec<String>| {
        if !current.trim().is_empty() {
            units.push(std::mem::take(current));
        }
        current.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut current, &mut units);
            in_fence = !in_fence;
            continue;
        }
        let starts_unit = in_fence
            || trimmed.is_empty()
            || trimmed.starts_with('|')
            || trimmed.starts_with('#')
            || trimmed.starts_with('>')
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed
                .split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if starts_unit {
            flush(&mut current, &mut units);
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(trimmed);
        if in_fence || trimmed.starts_with('|') || trimmed.starts_with('#') {
            flush(&mut current, &mut units);
        }
    }
    flush(&mut current, &mut units);

    let mut out = Vec::new();
    for unit in units {
        let mut rest = unit.as_str();
        while let Some(end) = [". ", "? ", "! "]
            .iter()
            .filter_map(|sep| rest.find(sep))
            .min()
        {
            out.push(rest[..=end].to_string());
            rest = &rest[end + 2..];
        }
        if !rest.trim().is_empty() {
            out.push(rest.to_string());
        }
    }
    out
}

/// The sentence as matched: lowercase with Markdown emphasis (`*`) and code
/// ticks removed, so `**legs** are` reads `legs are`.
fn plain(sentence: &str) -> String {
    sentence
        .to_lowercase()
        .chars()
        .filter(|c| *c != '*' && *c != '`')
        .collect()
}

/// The sentence's prose: [`plain`] without inline code spans or link
/// targets, for whole-word bans.
fn prose(sentence: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut rest = sentence;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            in_code = !in_code;
            out.push(' ');
        } else if !in_code && rest.starts_with("](") {
            match rest.find(')') {
                Some(close) => {
                    out.push(' ');
                    rest = &rest[close + 1..];
                    continue;
                }
                None => out.push(c),
            }
        } else if !in_code {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    plain(&out)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `fragment` appears in `text` as a whole word (or its plural).
fn has_word(text: &str, fragment: &str) -> bool {
    text.match_indices(fragment).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let mut after = text[at + fragment.len()..].chars();
        let next = match after.next() {
            Some('s') => after.next(),
            other => other,
        };
        !before.is_some_and(is_word_char) && !next.is_some_and(is_word_char)
    })
}

/// The sentence marks itself as history with a leading tag or a
/// parenthetical note; an everyday word such as `removed` does not count.
fn is_history(plain: &str) -> bool {
    let lead = plain.trim_start_matches(|c: char| {
        c.is_whitespace() || c.is_ascii_digit() || matches!(c, '-' | '>' | '#' | '|' | '.' | ':')
    });
    let tagged = |unit: &str| {
        let lead = unit.trim_start_matches(|c: char| {
            c.is_whitespace() || c.is_ascii_digit() || matches!(c, '-' | '>' | '#' | '.' | ':')
        });
        HISTORY_TAGS.iter().any(|tag| lead.starts_with(tag))
    };
    // A table row is history when one of its cells opens with a tag.
    tagged(lead)
        || lead.split('|').any(tagged)
        || HISTORY_NOTES.iter().any(|note| plain.contains(note))
}

/// The bans a sentence breaks, after the explicit exemptions.
fn broken_bans(file: &str, sentence: &str) -> Vec<&'static Ban> {
    let lower = plain(sentence);
    if RELOCATION.iter().any(|m| lower.contains(m)) || is_history(&lower) {
        return Vec::new();
    }
    if ALLOWED
        .iter()
        .any(|(f, fragment)| *f == file && lower.contains(fragment))
    {
        return Vec::new();
    }
    let prose = prose(sentence);
    BANS.iter()
        .filter(|b| {
            if b.word {
                has_word(&prose, b.fragment)
            } else {
                lower.contains(b.fragment)
            }
        })
        .filter(|b| b.needs.is_empty() || b.needs.iter().any(|n| has_word(&prose, n)))
        .collect()
}

#[test]
fn core_docs_promise_no_ui_or_plugins() {
    let docs = scanned_docs();
    assert!(
        docs.len() > 10,
        "expected the front-door files and docs/, found {}",
        docs.len()
    );
    let mut violations = Vec::new();
    for (file, text) in &docs {
        for sentence in sentences(text) {
            for ban in broken_bans(file, &sentence) {
                violations.push(format!(
                    "{file}: \"{}\" ({}): {}",
                    ban.fragment,
                    ban.rule,
                    sentence.trim()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "core docs promise what core no longer is; rewrite each sentence (or point it \
         to Susi Commercial Apps):\n{}",
        violations.join("\n")
    );
}

/// Every allow-list entry must still match a sentence, so a stale exemption
/// cannot linger after its sentence is rewritten.
#[test]
fn every_allowed_fragment_still_matches_a_sentence() {
    let docs = scanned_docs();
    for (file, fragment) in ALLOWED {
        let (_, text) = docs
            .iter()
            .find(|(f, _)| f == file)
            .unwrap_or_else(|| panic!("allow-listed file {file} is not scanned"));
        assert!(
            sentences(text).iter().any(|s| plain(s).contains(fragment)),
            "stale allow-list entry: {file} no longer contains `{fragment}`"
        );
    }
}

/// The scanner itself: planted promises are caught, the exemptions hold, and
/// one sentence's marker never leaks into the next.
#[test]
fn scanner_catches_planted_promises_and_honours_exemptions() {
    for planted in [
        "Open the CEO cockpit in your browser.",
        "Core seeds the default extension pack on first run.",
        "The daemon downloads automatically a model at boot.",
        "Core publishes a macOS release for every tag.",
        "Drop into the interactive shell with `susi shell`.",
        "| Plugin protocol MCP | ~22 leading servers (top: Filesystem) |",
        // Natural phrasings of each banned promise.
        "Core ships for Windows, Linux and macOS.",
        "Download the macOS build from the release page.",
        "Releases publish aarch64-unknown-linux-gnu and aarch64-pc-windows-msvc archives.",
        "Core ships a GUI for monitoring missions.",
        "Open the dashboard at http://127.0.0.1:9181.",
        "Open the harness cockpit in your browser.",
        "Core ships bundled plugins for GitHub and Slack.",
        "The daemon downloads the starter model at boot.",
        "Susi will automatically download a model on first run.",
        "The daemon auto-starts Ollama at boot.",
        "Core auto-installs leading MCP servers.",
        // Emphasis must not split a banned phrase.
        "- **The release's slowest legs** are macOS and Windows runners.",
        "Open the **web** UI on port 9181.",
        // Everyday words and bare suite mentions are not exemptions.
        "Core ships a web UI; removed sessions are hidden.",
        "Core's web UI mirrors the commercial desktop.",
        "The retired panel lives on as the web UI.",
    ] {
        let hits: Vec<_> = sentences(planted)
            .iter()
            .flat_map(|s| broken_bans("README.md", s))
            .collect();
        assert!(!hits.is_empty(), "planted promise not caught: {planted}");
    }
    for exempt in [
        "The visual cockpit lives in Susi Commercial Apps, not the web UI of core.",
        "RETRACTED: the harness plugin registry (`plugins.rs`) was removed.",
        "| 5 | Retracted: the plugin manifest (`plugins.rs`) |",
        "Known gap: the daemon starts a background download of the starter model.",
        "Releases built a macOS binary (historical: before 2026-11-03).",
        "macOS is a portable source target.",
        "The `cockpit` module serves [the API](../crates/susi-harness/src/cockpit.rs).",
        "Read the user guide.",
    ] {
        let hits: Vec<_> = sentences(exempt)
            .iter()
            .flat_map(|s| broken_bans("README.md", s))
            .collect();
        let fragments: Vec<_> = hits.iter().map(|b| b.fragment).collect();
        assert!(
            hits.is_empty(),
            "exempt sentence flagged by {fragments:?}: {exempt}"
        );
    }
    // The history marker of the first sentence must not cover the second.
    let text = "Retracted: the old panel. Open the web UI on port 9181.";
    let hits: Vec<_> = sentences(text)
        .iter()
        .flat_map(|s| broken_bans("README.md", s))
        .collect();
    assert_eq!(hits.len(), 1, "{text}");
    // Table rows are separate units.
    let table = "| a | retracted |\n| b | the web UI |\n";
    let hits: Vec<_> = sentences(table)
        .iter()
        .flat_map(|s| broken_bans("README.md", s))
        .collect();
    assert_eq!(hits.len(), 1, "{table}");
}

#[test]
fn prose_and_has_word_strip_inline_code_and_markdown_links() {
    let raw = "See [guide](https://example.com/gui/dashboard) and `cockpit` module.";
    let stripped = prose(raw);
    assert!(!has_word(&stripped, "gui"));
    assert!(!has_word(&stripped, "dashboard"));
    assert!(!has_word(&stripped, "cockpit"));
    assert!(broken_bans("README.md", raw).is_empty());

    assert!(has_word("dashboards are external", "dashboard"));
    assert!(!has_word("guided tour", "gui"));
    assert!(is_history(&plain("1. Retracted: old item")));
    assert!(is_history(&plain("Note (superseded by v2)")));
    assert!(!is_history(&plain("Normal sentence about architecture")));
}

#[test]
fn plain_strips_markdown_asterisks_and_backticks_and_lowercases() {
    assert_eq!(plain("**Bold** and `code`"), "bold and code");
    assert_eq!(plain("*Italic* _text_"), "italic _text_");
}
