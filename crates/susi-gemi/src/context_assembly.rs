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

impl AssembledContext {
    /// Verify structural and mathematical invariants on an assembled context.
    pub fn verify_context_invariants(&self) -> Result<(), String> {
        if self.total_tokens == 0 {
            return Err("AssembledContext total_tokens must be > 0".to_string());
        }
        if self.stable_prefix_tokens > self.total_tokens {
            return Err(format!(
                "AssembledContext stable_prefix_tokens ({}) must be <= total_tokens ({})",
                self.stable_prefix_tokens, self.total_tokens
            ));
        }
        if !self.cacheable_fraction.is_finite() || !(0.0..=1.0).contains(&self.cacheable_fraction) {
            return Err(format!(
                "AssembledContext cacheable_fraction ({}) must be finite and in 0.0..=1.0",
                self.cacheable_fraction
            ));
        }
        let under_threshold = self.stable_prefix_tokens < CACHEABLE_PREFIX_MIN_TOKENS;
        let has_threshold_defect = self.defects.contains(&CacheDefect::PrefixUnderThreshold);
        if under_threshold != has_threshold_defect {
            return Err(format!(
                "AssembledContext PrefixUnderThreshold defect ({has_threshold_defect}) disagrees with stable_prefix_tokens ({})",
                self.stable_prefix_tokens
            ));
        }
        for (i, d) in self.defects.iter().enumerate() {
            if self.defects[..i].contains(d) {
                return Err(format!("AssembledContext duplicate defect '{}'", d.label()));
            }
        }
        Ok(())
    }
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

impl CacheRecord {
    /// Verify structural invariants on a persisted context-cache journal record.
    pub fn verify_record_invariants(&self) -> Result<(), String> {
        if self.unix == 0 {
            return Err("CacheRecord unix timestamp must be > 0".to_string());
        }
        if let Some(provider) = &self.provider {
            if provider.trim().is_empty() {
                return Err("CacheRecord provider must not be blank when present".to_string());
            }
        }
        if self.total_tokens == 0 {
            return Err("CacheRecord total_tokens must be > 0".to_string());
        }
        if self.stable_prefix_tokens > self.total_tokens {
            return Err(format!(
                "CacheRecord stable_prefix_tokens ({}) must be <= total_tokens ({})",
                self.stable_prefix_tokens, self.total_tokens
            ));
        }
        if !self.cacheable_fraction.is_finite() || !(0.0..=1.0).contains(&self.cacheable_fraction) {
            return Err(format!(
                "CacheRecord cacheable_fraction ({}) must be finite and in 0.0..=1.0",
                self.cacheable_fraction
            ));
        }
        if let Some(saving) = self.estimated_saving_usd {
            if !saving.is_finite() || saving < 0.0 {
                return Err(format!(
                    "CacheRecord estimated_saving_usd ({saving}) must be finite and >= 0.0"
                ));
            }
        }
        for defect in &self.defects {
            if defect.trim().is_empty() {
                return Err("CacheRecord defect label must not be empty".to_string());
            }
        }
        Ok(())
    }
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

impl CacheReport {
    /// Verify structural invariants on an aggregated cache report.
    pub fn verify_report_invariants(&self) -> Result<(), String> {
        if !self.mean_cacheable_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.mean_cacheable_fraction)
        {
            return Err(format!(
                "CacheReport mean_cacheable_fraction ({}) must be finite and in 0.0..=1.0",
                self.mean_cacheable_fraction
            ));
        }
        if !self.total_estimated_saving_usd.is_finite() || self.total_estimated_saving_usd < 0.0 {
            return Err(format!(
                "CacheReport total_estimated_saving_usd ({}) must be finite and >= 0.0",
                self.total_estimated_saving_usd
            ));
        }
        if self.calls_below_cache_threshold > self.calls {
            return Err(format!(
                "CacheReport calls_below_cache_threshold ({}) must be <= calls ({})",
                self.calls_below_cache_threshold, self.calls
            ));
        }
        if self.calls == 0
            && (self.mean_cacheable_fraction != 0.0
                || self.total_estimated_saving_usd != 0.0
                || !self.defects.is_empty())
        {
            return Err(
                "CacheReport with 0 calls must have zeroed fraction, saving, and defects"
                    .to_string(),
            );
        }
        for (defect, count) in &self.defects {
            if defect.trim().is_empty() {
                return Err("CacheReport defect key must not be empty".to_string());
            }
            if *count == 0 || *count > self.calls {
                return Err(format!(
                    "CacheReport defect '{defect}' count ({count}) must be in 1..={}",
                    self.calls
                ));
            }
        }
        Ok(())
    }
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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn approx_tokens_enforces_minimum_one_and_scales_by_quarter_chars() {
        assert_eq!(approx_tokens(""), 1);
        assert_eq!(approx_tokens("abc"), 1);
        assert_eq!(approx_tokens("abcd"), 1);
        assert_eq!(approx_tokens(&"x".repeat(40)), 10);
    }

    #[test]
    fn cache_defect_labels_are_distinct_and_kebab_case() {
        let d1 = CacheDefect::LateInstructionReordered.label();
        let d2 = CacheDefect::NoInstructionPrefix.label();
        let d3 = CacheDefect::PrefixUnderThreshold.label();
        assert_eq!(d1, "late-instruction-reordered");
        assert_eq!(d2, "no-instruction-prefix");
        assert_eq!(d3, "prefix-under-threshold");
        assert_ne!(d1, d2);
        assert_ne!(d2, d3);
    }

    #[test]
    fn assemble_hoists_late_system_turns_and_flags_defects() {
        let turns = vec![
            ("user".to_string(), "hello".to_string()),
            ("system".to_string(), "be concise".to_string()),
            ("assistant".to_string(), "hi".to_string()),
        ];
        let ctx = assemble(&turns);
        assert!(ctx.prompt.starts_with("system: be concise\n\nuser: hello"));
        assert!(ctx.defects.contains(&CacheDefect::LateInstructionReordered));
        assert!(ctx.defects.contains(&CacheDefect::PrefixUnderThreshold));
        assert!(!ctx.defects.contains(&CacheDefect::NoInstructionPrefix));
    }

    #[test]
    fn record_assembly_at_and_cache_report_at_aggregate_records() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("context_cache.jsonl");
        let turns = vec![
            ("user".to_string(), "first turn".to_string()),
            ("assistant".to_string(), "second turn".to_string()),
        ];
        let ctx = assemble(&turns);
        record_assembly_at(&journal, None, &ctx);
        record_assembly_at(&journal, Some("unpriced-provider-xyz"), &ctx);
        let report = cache_report_at(&journal);
        assert_eq!(report.calls, 2);
        assert_eq!(report.calls_below_cache_threshold, 2);
        assert!((report.mean_cacheable_fraction - ctx.cacheable_fraction).abs() < 1e-6);
        assert_eq!(
            report.defects.get(CacheDefect::NoInstructionPrefix.label()),
            Some(&2)
        );
    }

    #[test]
    fn assemble_large_system_prefix_clears_under_threshold_defect_and_missing_journal_is_empty() {
        let large_system = "S".repeat(4_200);
        let turns = vec![
            ("system".to_string(), large_system),
            ("user".to_string(), "question".to_string()),
        ];
        let ctx = assemble(&turns);
        assert!(ctx.defects.is_empty(), "defects={:?}", ctx.defects);
        assert!(ctx.stable_prefix_tokens >= 1_024);

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nonexistent_context_cache.jsonl");
        let report = cache_report_at(&missing);
        assert_eq!(report.calls, 0);
        assert!(report.defects.is_empty());
    }

    #[test]
    fn verify_context_record_and_report_invariants_reject_corrupt_states() {
        let turns = vec![
            ("system".to_string(), "be concise".to_string()),
            ("user".to_string(), "hello".to_string()),
        ];
        let valid_ctx = assemble(&turns);
        assert!(valid_ctx.verify_context_invariants().is_ok());

        let mut zero_total = valid_ctx.clone();
        zero_total.total_tokens = 0;
        zero_total.stable_prefix_tokens = 0;
        assert!(zero_total.verify_context_invariants().is_err());

        let mut prefix_gt_total = valid_ctx.clone();
        prefix_gt_total.stable_prefix_tokens = prefix_gt_total.total_tokens + 1;
        assert!(prefix_gt_total.verify_context_invariants().is_err());

        let mut bad_fraction = valid_ctx.clone();
        bad_fraction.cacheable_fraction = 1.2;
        assert!(bad_fraction.verify_context_invariants().is_err());

        let mut missing_threshold_defect = valid_ctx.clone();
        missing_threshold_defect.defects.clear();
        assert!(missing_threshold_defect
            .verify_context_invariants()
            .is_err());

        let mut dup_defect = valid_ctx;
        dup_defect.defects.push(CacheDefect::PrefixUnderThreshold);
        assert!(dup_defect.verify_context_invariants().is_err());

        let valid_record = CacheRecord {
            unix: 1_700_000_000,
            provider: Some("deepseek".to_string()),
            stable_prefix_tokens: 512,
            total_tokens: 1_024,
            cacheable_fraction: 0.5,
            estimated_saving_usd: Some(0.001),
            defects: vec!["prefix-under-threshold".to_string()],
        };
        assert!(valid_record.verify_record_invariants().is_ok());

        let mut zero_unix = valid_record.clone();
        zero_unix.unix = 0;
        assert!(zero_unix.verify_record_invariants().is_err());

        let mut blank_provider = valid_record.clone();
        blank_provider.provider = Some("  ".to_string());
        assert!(blank_provider.verify_record_invariants().is_err());

        let mut neg_saving = valid_record.clone();
        neg_saving.estimated_saving_usd = Some(-0.01);
        assert!(neg_saving.verify_record_invariants().is_err());

        let mut blank_defect = valid_record;
        blank_defect.defects.push(String::new());
        assert!(blank_defect.verify_record_invariants().is_err());

        assert!(CacheReport::default().verify_report_invariants().is_ok());
        let valid_report = CacheReport {
            calls: 4,
            mean_cacheable_fraction: 0.5,
            total_estimated_saving_usd: 0.01,
            calls_below_cache_threshold: 2,
            defects: std::collections::BTreeMap::from([("prefix-under-threshold".to_string(), 2)]),
        };
        assert!(valid_report.verify_report_invariants().is_ok());

        let mut bad_below = valid_report.clone();
        bad_below.calls_below_cache_threshold = 5;
        assert!(bad_below.verify_report_invariants().is_err());

        let mut bad_zero_calls = CacheReport::default();
        bad_zero_calls.mean_cacheable_fraction = 0.5;
        assert!(bad_zero_calls.verify_report_invariants().is_err());

        let mut bad_defect_count = valid_report;
        bad_defect_count.defects.insert("overflow".to_string(), 5);
        assert!(bad_defect_count.verify_report_invariants().is_err());
    }
}
