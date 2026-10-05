//! Cache-first context assembly, measured (VC-202-006).
//!
//! Provider prompt caches (DeepSeek, OpenAI, Anthropic) hit on shared
//! prefixes: the portion of this call's prompt that is byte-identical to
//! the head of the previous call in the same conversation is served at the
//! cache-hit rate, the rest at the miss rate. Assembly therefore puts the
//! stable content first — instruction turns (system/developer) hoisted in
//! arrival order, then the conversation verbatim — so every subsequent turn
//! of a conversation extends the same prefix instead of rebuilding it.
//!
//! A shape that defeats caching is a defect with evidence: each assembled
//! context records its cacheable fraction, its priced saving estimate and
//! the defects it carried into `context_cache.jsonl`, and
//! [`cache_report`] reports the measured hit rate and saving.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Provider prefix caches only engage above roughly a kilobyte of shared
/// prefix; below that threshold "cacheable" content still pays miss rate.
const CACHEABLE_PREFIX_MIN_TOKENS: u64 = 1024;

/// A defect recorded when the assembled shape would not hit the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CacheDefect {
    /// An instruction turn (system/developer) arrived after conversation
    /// turns; a literal-order flatten would break the shared prefix, so
    /// assembly hoisted it. The input order itself was the defect.
    LateInstructionReordered,
    /// No instruction turns at all — the only shared content is history,
    /// so the first turn of every conversation pays full miss rate.
    NoInstructionPrefix,
    /// The shared prefix is under the provider cache threshold — even a
    /// perfectly ordered context buys nothing at this size.
    PrefixUnderThreshold,
}

impl CacheDefect {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::LateInstructionReordered => "late-instruction-reordered",
            Self::NoInstructionPrefix => "no-instruction-prefix",
            Self::PrefixUnderThreshold => "prefix-under-threshold",
        }
    }
}

/// The assembled prompt plus its measured cache profile.
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledContext {
    pub prompt: String,
    /// Approx tokens in the shared prefix: instruction block plus every
    /// turn except the newest — the portion a repeat call serves from
    /// cache instead of re-pricing.
    pub stable_prefix_tokens: u64,
    /// Approx tokens in the whole assembled prompt.
    pub total_tokens: u64,
    /// `stable_prefix_tokens / total_tokens` — the measured hit rate this
    /// call would see against its own conversation.
    pub cacheable_fraction: f64,
    pub defects: Vec<CacheDefect>,
}

/// Same ~4-chars-per-token estimate the server bills with.
#[must_use]
pub fn approx_tokens(text: &str) -> u64 {
    (text.chars().count() as u64 / 4).max(1)
}

fn is_instruction(role: &str) -> bool {
    matches!(role, "system" | "developer")
}

/// Assemble `(role, content)` turns into a cache-safe prompt.
///
/// Instruction turns are hoisted to the head in arrival order; conversation
/// turns keep their order. The wire shape (`role: content`, joined by a
/// blank line) is unchanged, so turn N+1 of a conversation is byte-for-byte
/// the prompt of turn N plus the new tail.
#[must_use]
pub fn assemble(turns: &[(String, String)]) -> AssembledContext {
    let mut defects = Vec::new();
    let first_conversation = turns.iter().position(|(role, _)| !is_instruction(role));
    if let Some(boundary) = first_conversation {
        if turns[boundary..]
            .iter()
            .any(|(role, _)| is_instruction(role))
        {
            defects.push(CacheDefect::LateInstructionReordered);
        }
    }
    let instructions: Vec<&(String, String)> = turns
        .iter()
        .filter(|(role, _)| is_instruction(role))
        .collect();
    if instructions.is_empty() {
        defects.push(CacheDefect::NoInstructionPrefix);
    }
    let ordered: Vec<&(String, String)> = instructions
        .iter()
        .copied()
        .chain(turns.iter().filter(|(role, _)| !is_instruction(role)))
        .collect();
    // The shared prefix is everything but the newest turn: what a repeat
    // call already paid for. Fewer than two turns share nothing.
    let (stable_turns, _) = ordered.split_at(ordered.len().saturating_sub(1));
    let stable_prefix_tokens = if stable_turns.is_empty() {
        0
    } else {
        stable_turns
            .iter()
            .map(|(role, content)| approx_tokens(&format!("{role}: {content}")) + 1)
            .sum()
    };
    let prompt = ordered
        .iter()
        .map(|(role, content)| format!("{role}: {content}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let total_tokens = approx_tokens(&prompt);
    let cacheable_fraction = if total_tokens == 0 {
        0.0
    } else {
        (stable_prefix_tokens as f64 / total_tokens as f64).min(1.0)
    };
    if stable_prefix_tokens < CACHEABLE_PREFIX_MIN_TOKENS {
        defects.push(CacheDefect::PrefixUnderThreshold);
    }
    AssembledContext {
        prompt,
        stable_prefix_tokens,
        total_tokens,
        cacheable_fraction,
        defects,
    }
}

/// One measured assembly — payload-free: token counts, priced saving and
/// defect labels only, never prompt text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRecord {
    pub unix: u64,
    pub provider: Option<String>,
    pub stable_prefix_tokens: u64,
    pub total_tokens: u64,
    pub cacheable_fraction: f64,
    /// The USD delta between serving the shared prefix at the provider's
    /// cache-hit rate and its miss rate — `None` when the provider has no
    /// price record. Estimated, never provider-reported.
    pub estimated_saving_usd: Option<f64>,
    pub defects: Vec<String>,
}

#[must_use]
pub fn default_journal_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("context_cache.jsonl")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Price the saving the shared prefix realises on `provider`: hit-priced
/// vs miss-priced prefix tokens against the installed catalog. `None` when
/// the provider is unpriced.
#[must_use]
pub fn estimated_saving_usd(provider: &str, stable_prefix_tokens: u64) -> Option<f64> {
    let catalog = crate::engines::cost::price_catalog();
    let at_hit = crate::engines::cost::expected_call_cost_usd_in(
        catalog.as_ref(),
        provider,
        stable_prefix_tokens,
        0,
        1.0,
    )?;
    let at_miss = crate::engines::cost::expected_call_cost_usd_in(
        catalog.as_ref(),
        provider,
        stable_prefix_tokens,
        0,
        0.0,
    )?;
    Some((at_miss - at_hit).max(0.0))
}

/// Append the measurement for one assembled context. `provider` prices the
/// saving estimate; pass the provider the placement chose.
pub fn record_assembly_at(path: &Path, provider: Option<&str>, ctx: &AssembledContext) {
    let record = CacheRecord {
        unix: unix_now(),
        provider: provider.map(str::to_string),
        stable_prefix_tokens: ctx.stable_prefix_tokens,
        total_tokens: ctx.total_tokens,
        cacheable_fraction: ctx.cacheable_fraction,
        estimated_saving_usd: provider
            .and_then(|p| estimated_saving_usd(p, ctx.stable_prefix_tokens)),
        defects: ctx
            .defects
            .iter()
            .map(CacheDefect::label)
            .map(str::to_string)
            .collect(),
    };
    if let Ok(line) = serde_json::to_string(&record) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }
}

/// As [`record_assembly_at`] against the shared journal.
pub fn record_assembly(provider: Option<&str>, ctx: &AssembledContext) {
    record_assembly_at(&default_journal_path(), provider, ctx);
}

/// The measured picture over the whole journal: how much of what we send
/// is cache-served and what the shaping has saved so far.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheReport {
    pub calls: u64,
    pub mean_cacheable_fraction: f64,
    pub total_estimated_saving_usd: f64,
    pub calls_below_cache_threshold: u64,
    pub defects: std::collections::BTreeMap<String, u64>,
}

#[must_use]
pub fn cache_report_at(path: &Path) -> CacheReport {
    let mut report = CacheReport::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return report;
    };
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<CacheRecord>(line) else {
            continue;
        };
        report.calls += 1;
        report.mean_cacheable_fraction += record.cacheable_fraction;
        if let Some(saving) = record.estimated_saving_usd {
            report.total_estimated_saving_usd += saving;
        }
        for defect in record.defects {
            if defect == CacheDefect::PrefixUnderThreshold.label() {
                report.calls_below_cache_threshold += 1;
            }
            *report.defects.entry(defect).or_default() += 1;
        }
    }
    if report.calls > 0 {
        report.mean_cacheable_fraction /= report.calls as f64;
    }
    report
}

/// As [`cache_report_at`] against the shared journal.
#[must_use]
pub fn cache_report() -> CacheReport {
    cache_report_at(&default_journal_path())
}
