//! Retired-model detection from `/models` diffs and remap proposals
//! (T-CLAUDE-48): diff a vendor's live catalogue against the configured
//! default, and when the configured id vanished propose (or apply) a remap —
//! replacing the hardcoded retired table in `cloud.rs` with evidence-driven
//! decisions.

/// Result of diffing a recorded model set against a live listing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelsDiff {
    /// Present in live but not recorded.
    pub added: Vec<String>,
    /// Recorded but gone from live (retired candidates).
    pub removed: Vec<String>,
}

/// ids removed from `recorded` in `live`.
pub fn diff_models(recorded: &[String], live: &[String]) -> ModelsDiff {
    let mut d = ModelsDiff::default();
    for m in recorded {
        if !live.iter().any(|l| l == m) {
            d.removed.push(m.clone());
        }
    }
    for m in live {
        if !recorded.iter().any(|r| r == m) {
            d.added.push(m.clone());
        }
    }
    d.added.sort();
    d.removed.sort();
    d
}

/// Pick a replacement for `vendor`'s retired `model` from the live list —
/// same family first, then the vendor default ranking.
pub fn propose_remap(vendor: &str, retired_model: &str, live: &[String]) -> Option<String> {
    if live.iter().any(|m| m == retired_model) {
        return None; // not actually retired
    }
    // same-family candidates share a name stem before the last '-' segment
    let stem = retired_model
        .rsplit_once('-')
        .map_or(retired_model, |x| x.0);
    let mut same_family: Vec<&String> = live
        .iter()
        .filter(|m| m.starts_with(stem) && m.as_str() != retired_model)
        .collect();
    same_family.sort();
    if let Some(m) = same_family.last() {
        return Some((*m).clone());
    }
    crate::default_models::default_model(vendor, live)
}

/// Data-driven replacement for `cloud.rs`'s hardcoded retired table: given a
/// vendor's endpoint name, its configured model, and the live `/models`
/// listing, return the id the endpoint should advertise now.
///
/// When no live listing is available (`live == None`) the bundled retirement
/// table is the fallback so offline config still gets sane values.
pub fn remap_endpoint_model(
    vendor_name: &str,
    configured: &str,
    live: Option<&[String]>,
) -> String {
    if let Some(live) = live {
        if live.iter().any(|m| m == configured) {
            return configured.into();
        }
        if let Some(next) = propose_remap(vendor_name, configured, live) {
            return next;
        }
    }
    // offline fallback: last-known retirement facts
    bundled_replacement(vendor_name, configured)
        .map(str::to_string)
        .unwrap_or_else(|| configured.to_string())
}

/// Last-known retirement facts (same data the cloud.rs table held, kept as
/// the offline fallback until a live diff proves otherwise).
pub fn bundled_replacement(vendor_name: &str, model: &str) -> Option<&'static str> {
    let name = vendor_name.to_ascii_lowercase();
    match (name.as_str(), model) {
        (
            "groq",
            "llama-3.3-70b-versatile"
            | "llama-3.1-8b-instant"
            | "llama-3.1-70b-versatile"
            | "mixtral-8x7b-32768",
        ) => Some("openai/gpt-oss-20b"),
        (
            "googlegemini" | "gemini" | "google",
            "gemini-2.0-pro" | "gemini-1.5-pro" | "gemini-2.5-pro" | "gemini-2.5-flash"
            | "gemini-2.0-flash" | "gemini-1.5-flash",
        ) => Some("gemini-3.6-flash"),
        ("deepseek", "deepseek-reasoner" | "deepseek-r1") => Some("deepseek-chat"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn model_deprecation_diff_reports_added_and_removed() {
        let recorded = ids(&["a", "b", "c"]);
        let live = ids(&["b", "c", "d"]);
        let d = diff_models(&recorded, &live);
        assert_eq!(d.removed, ["a"]);
        assert_eq!(d.added, ["d"]);
    }

    #[test]
    fn model_deprecation_deepseek_chat_case() {
        // the motivating incident: deepseek-chat retired → live has newer ids
        let live = ids(&["deepseek-chat-v3", "deepseek-reasoner-v2"]);
        let remap = propose_remap("deepseek", "deepseek-chat", &live).unwrap();
        assert!(remap.starts_with("deepseek"));
    }

    #[test]
    fn model_deprecation_present_model_never_remapped() {
        let live = ids(&["gpt-5", "gpt-4o"]);
        assert!(propose_remap("openai", "gpt-5", &live).is_none());
        assert_eq!(
            remap_endpoint_model("openai", "gpt-5", Some(&live)),
            "gpt-5"
        );
    }

    #[test]
    fn model_deprecation_retired_falls_to_vendor_default() {
        let live = ids(&["mistral-large-2411", "mistral-small-2503"]);
        let remap = propose_remap("mistral", "mistral-large-2410", &live);
        // same-family prefer mistral-large-2411
        assert_eq!(remap.as_deref(), Some("mistral-large-2411"));
    }

    #[test]
    fn model_deprecation_offline_uses_bundled_table() {
        assert_eq!(
            remap_endpoint_model("groq", "llama-3.3-70b-versatile", None),
            "openai/gpt-oss-20b"
        );
        assert_eq!(
            remap_endpoint_model("groq", "openai/gpt-oss-20b", None),
            "openai/gpt-oss-20b"
        );
    }

    #[test]
    fn model_deprecation_live_evidence_beats_table() {
        // table says groq llama-3.3 retired; live still lists it → keep it
        let live = ids(&["llama-3.3-70b-versatile"]);
        assert_eq!(
            remap_endpoint_model("groq", "llama-3.3-70b-versatile", Some(&live)),
            "llama-3.3-70b-versatile"
        );
    }
}
