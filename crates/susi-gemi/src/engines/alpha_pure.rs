//! Candle-free reflex features shared by the local engine and the slim core.
//!
//! Fleet recruitment and Tier-0 features do not need Candle. Training and
//! serving the classifier do, and those stay behind `local-engine`.

use susi_error::EaiResult as Result;

pub(crate) const DIM: usize = 128;

pub fn semantic_centroid_projection(
    prompt: &str,
    anchors: Option<&[crate::susi_core::AgentProfile]>,
) -> Result<Vec<f32>> {
    let start = std::time::Instant::now();
    let mut vec = vec![0.0f32; DIM];
    let prompt_lower = prompt.to_lowercase();
    let words: Vec<&str> = prompt_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();

    if words.is_empty() {
        return Ok(vec);
    }

    for (i, word) in words.iter().enumerate() {
        let word_vec = get_semantic_anchor(word, anchors);
        for (j, &val) in word_vec.iter().enumerate() {
            let weight = 1.0 / (i as f32 + 1.0);
            vec[j] += val * weight;
        }
    }

    let sum_sq = vec.iter().map(|x| x * x).sum::<f32>();
    if sum_sq > 0.0 {
        let norm = sum_sq.sqrt();
        for x in vec.iter_mut() {
            *x /= norm;
        }
    }

    let _elapsed = start.elapsed();

    Ok(vec)
}

fn get_semantic_anchor(word: &str, anchors: Option<&[crate::susi_core::AgentProfile]>) -> Vec<f32> {
    let mut anchor = vec![0.0f32; DIM];

    // Zero-Lock Anchor Mapping
    if let Some(agent_profiles) = anchors {
        for agent in agent_profiles {
            if agent.semantic_anchors.iter().any(|a| a == word) {
                let offset = 80 + (agent.name.len() % 40);
                anchor[offset] = 1.0;
                return anchor;
            }
        }
    }

    if let Some(category) = anchor_category(word) {
        for val in anchor.iter_mut().skip(category * 10).take(10) {
            *val = 1.0;
        }
    } else {
        anchor[(fnv1a(word, 0) as usize) % DIM] = 0.5;
    }
    anchor
}

/// The reflex classifier's input features. `semantic_centroid_projection`
/// is tuned for fleet recruitment (a category word lights a 10-dim band
/// at 1.0 while any other word lights one dim at 0.5, first word weighted
/// most), and under it a leading category word decides everything:
/// "write a poem about the ocean" projected within 0.01 of "write notes
/// to todo.md" and was served `write_file` at 0.84 confidence. Here
/// stopwords are dropped and every remaining word contributes equal
/// norm, position-independent — a category word spread over its band,
/// any other word over two FNV-1a buckets — so content words count as
/// much as the verb.
pub fn reflex_features(prompt: &str) -> Vec<f32> {
    let mut vec = vec![0.0f32; DIM];
    let lower = prompt.to_lowercase();
    for word in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !REFLEX_STOPWORDS.contains(w))
    {
        let stem = stem(word);
        if let Some(category) = anchor_category(word)
            .or_else(|| stem_category(&stem))
            .or_else(|| synonym_category(&stem))
        {
            let weight = 1.0 / (10f32).sqrt();
            for val in vec.iter_mut().skip(category * 10).take(10) {
                *val += weight;
            }
        } else {
            let weight = std::f32::consts::FRAC_1_SQRT_2;
            vec[(fnv1a(&stem, 0) as usize) % DIM] += weight;
            vec[(fnv1a(&stem, 1) as usize) % DIM] += weight;
        }
    }
    let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        vec.iter_mut().for_each(|x| *x /= norm);
    }
    vec
}

/// A light suffix stemmer for reflex features (not linguistics: it only has
/// to map inflections of one word to one key). Measured before it: 4 of 18
/// inflected paraphrases of trained intents ("running the tests", "listed
/// directories", "fixing the builds") were served — whole-word features made
/// "running" and "run" different words. Rules, first match wins, only on
/// words longer than 3 letters: `-ies`→`-y`, `-ing`/`-ed` (then undouble a
/// final doubled consonant), plural `-es` after s/x/z/ch/sh, plural `-s`
/// (not `-ss`/`-us`/`-is`); then a trailing `-e` is dropped so "save" and
/// "saving" meet at "sav".
pub(crate) fn stem(word: &str) -> String {
    let mut w = word.to_lowercase();
    if w.len() <= 3 || !w.is_ascii() {
        return w;
    }
    let undouble = |w: &mut String| {
        let bytes = w.as_bytes();
        let n = bytes.len();
        if n >= 2 && bytes[n - 1] == bytes[n - 2] && !b"aeioulsz".contains(&bytes[n - 1]) {
            w.pop();
        }
    };
    if let Some(root) = w.strip_suffix("ies") {
        w = format!("{root}y");
    } else if w.len() > 5 && w.ends_with("ing") {
        w.truncate(w.len() - 3);
        undouble(&mut w);
    } else if w.len() > 4 && w.ends_with("ed") {
        w.truncate(w.len() - 2);
        undouble(&mut w);
    } else if ["ses", "xes", "zes", "ches", "shes"]
        .iter()
        .any(|s| w.ends_with(s))
    {
        w.truncate(w.len() - 2);
    } else if w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") && !w.ends_with("is") {
        w.pop();
    }
    if w.len() > 3 && w.ends_with('e') {
        w.pop();
    }
    w
}

/// Category of a stemmed word, via the stems of the anchor words ("files" →
/// "fil" ← "file"), so inflections reach their category band.
pub(crate) const ANCHOR_WORDS: &[&str] = &[
    "status",
    "health",
    "state",
    "check",
    "hardware",
    "system",
    "report",
    "version",
    "ver",
    "build",
    "engine",
    "revision",
    "write",
    "save",
    "create",
    "file",
    "update",
    "put",
    "read",
    "get",
    "fetch",
    "cat",
    "show",
    "content",
    "list",
    "ls",
    "dir",
    "directory",
    "folder",
    "files",
    "scout",
    "search",
    "find",
    "look",
    "discover",
    "mcp",
    "reason",
    "think",
    "solve",
    "complex",
    "calculate",
    "fix",
    "heal",
    "repair",
    "audit",
    "compliance",
];

fn stem_category(stemmed: &str) -> Option<usize> {
    static STEMS: std::sync::LazyLock<std::collections::HashMap<String, usize>> =
        std::sync::LazyLock::new(|| {
            ANCHOR_WORDS
                .iter()
                .filter_map(|w| anchor_category(w).map(|c| (stem(w), c)))
                .collect()
        });
    STEMS.get(stemmed).copied()
}

/// Everyday verbs that mean an anchor category but are not in the anchor
/// list, which fleet recruitment shares and is left unchanged. Reflex
/// features only ("dump" was dropped after the tool-heavy benchmark: it
/// usually means *write out* — "dump the db to a file" was served
/// `read_file`). Measured before: 6 of 18 synonym paraphrases of trained
/// intents ("display the readme", "which release is this", "correct the
/// build") were served. The support gate still guards out-of-distribution
/// text: a shared verb alone is far below `SUPPORT_MIN`.
pub(crate) const SYNONYMS: &[(&str, usize)] = &[
    ("inspect", 0),
    ("diagnose", 0),
    ("uptime", 0),
    ("release", 1),
    ("store", 2),
    ("record", 2),
    ("persist", 2),
    ("append", 2),
    ("display", 3),
    ("view", 3),
    ("print", 3),
    ("open", 3),
    ("peek", 3),
    ("enumerate", 4),
    ("inside", 4),
    ("explore", 5),
    ("hunt", 5),
    ("browse", 5),
    ("locate", 5),
    ("ponder", 6),
    ("figure", 6),
    ("analyze", 6),
    ("deduce", 6),
    ("compute", 6),
    ("estimate", 6),
    ("correct", 7),
    ("patch", 7),
    ("mend", 7),
    ("debug", 7),
];

fn synonym_category(stemmed: &str) -> Option<usize> {
    static STEMS: std::sync::LazyLock<std::collections::HashMap<String, usize>> =
        std::sync::LazyLock::new(|| SYNONYMS.iter().map(|(w, c)| (stem(w), *c)).collect());
    STEMS.get(stemmed).copied()
}

/// Function words that carry no intent; dropped from reflex features so a
/// shared "the"/"a" never makes two unrelated prompts look alike.
pub(crate) const REFLEX_STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "at", "for", "with", "about", "into",
    "from", "by", "is", "are", "was", "be", "it", "this", "that", "these", "those", "me", "my",
    "i", "you", "your", "please", "can", "could", "would", "what", "which", "who", "how", "do",
    "does",
];

/// The 10-dim band a known intent word lights, if any.
pub(crate) fn anchor_category(word: &str) -> Option<usize> {
    match word {
        "status" | "health" | "state" | "check" | "hardware" | "system" | "report" => Some(0),
        "version" | "ver" | "build" | "engine" | "revision" => Some(1),
        "write" | "save" | "create" | "file" | "update" | "put" => Some(2),
        "read" | "get" | "fetch" | "cat" | "show" | "content" => Some(3),
        "list" | "ls" | "dir" | "directory" | "folder" | "files" => Some(4),
        "scout" | "search" | "find" | "look" | "discover" | "mcp" => Some(5),
        "reason" | "think" | "solve" | "complex" | "calculate" => Some(6),
        "fix" | "heal" | "repair" | "audit" | "compliance" => Some(7),
        _ => None,
    }
}

/// FNV-1a with a seed byte folded in first, so seeds 0 and 1 give two
/// independent buckets. The word hash was a byte *sum* before EV-CLAUDE-004,
/// which sent every anagram ("stop"/"post"/"pots"/"tops") to one bucket.
fn fnv1a(word: &str, seed: u8) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in std::iter::once(seed).chain(word.bytes()) {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_maps_inflections_to_one_key() {
        assert_eq!(stem("running"), "run");
        assert_eq!(stem("directories"), "directory");
        assert_eq!(stem("saving"), stem("save"));
        assert_eq!(stem("status"), "status");
    }

    #[test]
    fn centroid_projection_is_deterministic_and_unit_length() {
        let once = semantic_centroid_projection("check engine status", None).unwrap();
        let twice = semantic_centroid_projection("check engine status", None).unwrap();
        assert_eq!(once, twice);
        assert_eq!(once.len(), DIM);
        let norm: f32 = once.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{norm}");
    }

    #[test]
    fn an_empty_prompt_projects_to_zeros() {
        let vec = semantic_centroid_projection("...", None).unwrap();
        assert!(vec.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn reflex_features_drop_stopwords() {
        let with = reflex_features("the status");
        let bare = reflex_features("status");
        assert_eq!(with, bare);
    }

    #[test]
    fn anchor_category_bands_are_stable() {
        assert_eq!(anchor_category("status"), Some(0));
        assert_eq!(anchor_category("write"), Some(2));
        assert_eq!(anchor_category("ocean"), None);
    }
}
