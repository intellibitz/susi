//! Vendor changelog feeds -> knowledge-base proposals (VC-201-088 / T-CLAUDE-338).
//!
//! Several vendors publish RSS/Atom changelogs (OpenAI, Anthropic, GitHub
//! releases). This module extracts entries from a feed document with a
//! deliberately minimal tag scanner — no XML library, feeds are structurally
//! simple — classifies each entry and raises a [`Proposal`] carrying the
//! source link. Classification is keyword-based and conservative: anything
//! unclear lands in `Other` for human review rather than being guessed.

use crate::eco_openapi_ingest::Proposal;
use crate::eco_schema::Provenance;
use serde::{Deserialize, Serialize};

/// One feed entry, normalised across RSS `<item>` and Atom `<entry>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedEntry {
    pub title: String,
    pub link: String,
    /// `pubDate` / `published` / `updated` as the feed wrote it.
    pub published: String,
    pub summary: String,
}

/// How a changelog entry changes the ecosystem surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryClass {
    NewModel,
    Deprecation,
    BreakingChange,
    Other,
}

/// Pull the text of the first `<tag>` inside `body`, or `None`.
fn tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = body.find(&format!("<{name}"))?;
    let start = body[open..].find('>')? + open + 1;
    let close = body[start..].find(&format!("</{name}>"))? + start;
    Some(body[start..close].trim())
}

/// Strip `<![CDATA[ ... ]]>` wrappers from extracted text.
fn unescape(text: &str) -> String {
    text.trim()
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
}

/// Parse RSS `<item>`s and Atom `<entry>`s out of a feed document.
#[must_use]
pub fn parse_feed(doc: &str) -> Vec<FeedEntry> {
    let mut entries = Vec::new();
    // RSS: <item> ... </item>; Atom: <entry> ... </entry>
    for wrapper in ["item", "entry"] {
        let mut rest = doc;
        while let Some(start) = rest.find(&format!("<{wrapper}>")) {
            let Some(end) = rest[start..].find(&format!("</{wrapper}>")) else {
                break;
            };
            let body = &rest[start..start + end];
            rest = &rest[start + end..];
            let link = tag(body, "link")
                .map(unescape)
                .or_else(|| {
                    // Atom: <link href="..."/>
                    body.find("<link")
                        .and_then(|i| body[i..].find("href=\"").map(|h| i + h + 6))
                        .and_then(|i| body[i..].find('"').map(|e| body[i..i + e].to_string()))
                })
                .unwrap_or_default();
            entries.push(FeedEntry {
                title: tag(body, "title").map(unescape).unwrap_or_default(),
                link,
                published: tag(body, "pubDate")
                    .or_else(|| tag(body, "published"))
                    .or_else(|| tag(body, "updated"))
                    .map(unescape)
                    .unwrap_or_default(),
                summary: tag(body, "description")
                    .or_else(|| tag(body, "summary"))
                    .or_else(|| tag(body, "content"))
                    .map(unescape)
                    .unwrap_or_default(),
            });
        }
    }
    entries
}

/// Keyword classification — order matters: deprecation/breaking beat "new".
#[must_use]
pub fn classify(entry: &FeedEntry) -> EntryClass {
    let text = format!("{} {}", entry.title, entry.summary).to_lowercase();
    if text.contains("deprecat") || text.contains("sunset") || text.contains("retir") {
        EntryClass::Deprecation
    } else if text.contains("breaking")
        || text.contains("backward incompatib")
        || text.contains("removed")
    {
        EntryClass::BreakingChange
    } else if text.contains("new model")
        || text.contains("introducing")
        || text.contains("now available")
        || text.contains("launch")
    {
        EntryClass::NewModel
    } else {
        EntryClass::Other
    }
}

/// Raise a proposal from one entry. The feed URL is the provenance source.
#[must_use]
pub fn entry_proposal(subject: &str, entry: &FeedEntry, feed_src: &Provenance) -> Proposal {
    let kind = match classify(entry) {
        EntryClass::NewModel => "new-model",
        EntryClass::Deprecation => "deprecation",
        EntryClass::BreakingChange => "breaking-change",
        EntryClass::Other => "changelog-note",
    };
    Proposal {
        kind: kind.into(),
        subject: subject.to_string(),
        summary: entry.title.clone(),
        detail: serde_json::json!({
            "link": entry.link,
            "published": entry.published,
            "class": format!("{:?}", classify(entry)),
        }),
        provenance: feed_src.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Vendor changelog</title>
  <item>
    <title>GPT-4o now available in the API</title>
    <link>https://vendor.example/changelog/gpt-4o</link>
    <pubDate>Mon, 13 May 2024 00:00:00 GMT</pubDate>
    <description>Introducing our newest flagship model.</description>
  </item>
  <item>
    <title>Deprecating gpt-3.5-turbo-0301</title>
    <link>https://vendor.example/changelog/deprecate-0301</link>
    <pubDate>Mon, 01 Sep 2025 00:00:00 GMT</pubDate>
    <description>Sunset scheduled for 2026.</description>
  </item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <title>v2 responses: breaking change to tool schema</title>
    <link href="https://vendor.example/releases/v2"/>
    <updated>2025-11-01T00:00:00Z</updated>
    <summary>tool arguments are no longer strings.</summary>
  </entry>
</feed>"#;

    fn feed_src() -> Provenance {
        Provenance {
            source: "https://vendor.example/changelog.rss".into(),
            spec_version: "feed".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Reported,
        }
    }

    #[test]
    fn eco_changelog_feeds_parses_rss_items() {
        let entries = parse_feed(RSS);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "GPT-4o now available in the API");
        assert!(entries[0].link.contains("/gpt-4o"));
        assert!(entries[0].published.contains("2024"));
    }

    #[test]
    fn eco_changelog_feeds_parses_atom_entries() {
        let entries = parse_feed(ATOM);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].link.contains("/releases/v2"));
        assert!(entries[0].published.contains("2025-11-01"));
    }

    #[test]
    fn eco_changelog_feeds_classifies_entries() {
        let entries = parse_feed(RSS);
        assert_eq!(classify(&entries[0]), EntryClass::NewModel);
        assert_eq!(classify(&entries[1]), EntryClass::Deprecation);
        let atom = parse_feed(ATOM);
        assert_eq!(classify(&atom[0]), EntryClass::BreakingChange);
        let other = FeedEntry {
            title: "status page maintenance".into(),
            link: String::new(),
            published: String::new(),
            summary: "routine maintenance".into(),
        };
        assert_eq!(classify(&other), EntryClass::Other);
    }

    #[test]
    fn eco_changelog_feeds_entries_become_cited_proposals() {
        let entries = parse_feed(RSS);
        let p = entry_proposal("openai-api", &entries[1], &feed_src());
        assert_eq!(p.kind, "deprecation");
        assert!(p.detail["link"]
            .as_str()
            .unwrap()
            .contains("deprecate-0301"));
        assert!(p.provenance.source.contains("changelog.rss"));
        let p2 = entry_proposal("openai-api", &entries[0], &feed_src());
        assert_eq!(p2.kind, "new-model");
    }
}
