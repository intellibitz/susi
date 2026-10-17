//! Canonical types, state, and the shipped verification path for the
//! VC-203-001 normative requirements ledger.
//!
//! The ledger at `.agents/gmcp/requirements-ledger.json` is the single
//! definition of GMCP conformance (contract: `docs/gmcp/contract-203-001.md`).
//! This module is the production path the shipped runtime uses: it loads the
//! ledger, verifies every vendored spec file byte-for-byte against the pinned
//! manifest, and enforces the row-level invariants — verbatim clauses,
//! complete traces, and deterministic dispositions — so a tampered or
//! reclassified artifact fails closed anywhere GMCP runs, not only in the
//! authoring-time Python checker.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use susi_error::EaiError;

const LEDGER_SCHEMA: &str = "susi/gmcp-requirements/v1";
const SOURCES_SCHEMA: &str = "susi/gmcp-sources/v1";

/// Largest artifact the verifier holds in memory. The pinned corpus is ~3 MiB
/// today; a manifest claiming more is hostile or corrupt — verification must
/// bound reads rather than trusting the artifact.
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Largest requirement set the ledger may carry (~1.7k rows today). A ledger
/// grown past this bound is rejected before row iteration so a hostile or
/// corrupt artifact cannot turn verification into unbounded work.
const MAX_LEDGER_ROWS: usize = 8_192;

/// Confinement: manifest `document` names must stay inside the pinned tree.
/// A `..`, absolute path, backslash, NUL, or empty/`.` component would let
/// `Path::join` escape `.agents/gmcp/spec` (or the repo root for repository
/// documents), so verification fails closed on them.
fn confined_document(document: &str) -> Result<(), EaiError> {
    let escaped = document.is_empty()
        || document.starts_with('/')
        || document.contains('\\')
        || document.contains('\0')
        || document
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..");
    if escaped {
        return Err(EaiError::config(format!(
            "document path {document:?} is not confined to the pinned tree"
        )));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementsLedger {
    pub schema: String,
    pub version: String,
    pub baseline: LedgerBaseline,
    pub requirements: Vec<RequirementRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerBaseline {
    pub generated_unix: u64,
    pub contract: String,
    pub sources: String,
    pub revisions: BaselineRevisions,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineRevisions {
    pub modern: String,
    pub compatibility: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementRow {
    pub id: String,
    pub revision: String,
    pub categories: Vec<Category>,
    pub level: Level,
    pub source: RowSource,
    pub clause: String,
    pub disposition: Disposition,
    pub trace: Trace,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowSource {
    pub repository: String,
    pub document: String,
    pub sha256: String,
    pub line: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disposition {
    pub status: DispositionStatus,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    pub vector: String,
    pub owner: String,
    pub production_path: String,
    pub positive_test: String,
    pub negative_test: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Must,
    MustNot,
    Should,
    ShouldNot,
    May,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Behavior,
    Error,
    Deprecation,
    Security,
    Extension,
    Registry,
    Claim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispositionStatus {
    Modern,
    Adapter,
    Optional,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcesManifest {
    schema: String,
    generated_unix: u64,
    revisions: BaselineRevisions,
    upstream: serde_json::Value,
    files: Vec<SourceFile>,
    repository_documents: Vec<RepositoryDocument>,
    external_authorities: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    document: String,
    repo: String,
    upstream_path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryDocument {
    document: String,
    sha256: String,
    bytes: u64,
}

/// Aggregate verification result the CLI prints and callers can assert on.
#[derive(Debug)]
pub struct LedgerReport {
    pub rows: usize,
    pub documents: usize,
    pub revisions: BTreeMap<String, usize>,
    pub dispositions: BTreeMap<String, usize>,
    pub categories: BTreeMap<String, usize>,
    pub vectors: usize,
}

/// Canonical JSON rendering of a verification report. This is the single
/// producer every entry point consumes — `susi mcp ledger`, the
/// `ledger_verify` tool (direct call, daemon, gateway, and the served MCP
/// surface all dispatch to it), and tests — so each surface emits the
/// identical payload rather than assembling its own view.
pub fn report_json(report: &LedgerReport) -> serde_json::Value {
    serde_json::json!({
        "ledger": ".agents/gmcp/requirements-ledger.json",
        "rows": report.rows,
        "documents": report.documents,
        "revisions": report.revisions,
        "dispositions": report.dispositions,
        "categories": report.categories,
        "vectors": report.vectors,
        "verified": true
    })
}

/// Verify the pinned ledger under `root` and render the canonical report.
pub fn verify_report_json(root: &Path) -> Result<serde_json::Value, EaiError> {
    verify_pinned_ledger(root).map(|report| report_json(&report))
}

/// Bounded read for the verifier's own JSON artifacts — stat first so a
/// hostile or corrupt artifact is rejected before a single byte is mapped.
fn read(root: &Path, rel: &str) -> Result<Vec<u8>, EaiError> {
    let path = root.join(rel);
    let bytes = std::fs::metadata(&path)
        .map_err(|e| EaiError::io(format!("read {rel}: {e}")))?
        .len();
    if bytes > MAX_ARTIFACT_BYTES {
        return Err(EaiError::config(format!(
            "{rel} exceeds the {MAX_ARTIFACT_BYTES}-byte verification bound"
        )));
    }
    std::fs::read(&path).map_err(|e| EaiError::io(format!("read {rel}: {e}")))
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// One upstream-drift finding against a pinned source: either the refetched
/// bytes disagree with the pinned hash (`hash_drift`) or the refetch could
/// not produce classifiable bytes (`unfetchable`). Both kinds fail closed.
#[derive(Debug)]
pub struct DriftFinding {
    /// The manifest `document` name the finding belongs to.
    pub document: String,
    /// `hash_drift` or `unfetchable`.
    pub kind: &'static str,
    /// Human-readable diagnostic (status, hash mismatch, missing pin, ...).
    pub detail: String,
}

/// Result of an upstream-drift pass over `.agents/gmcp/sources.json`.
#[derive(Debug)]
pub struct DriftReport {
    /// Pinned documents checked.
    pub checked: usize,
    /// Documents whose refetched bytes match the pinned hash and size.
    pub matched: usize,
    /// Failures — every one fails the drift check closed.
    pub findings: Vec<DriftFinding>,
}

/// Per-request timeout for one upstream refetch, and the total wall-clock
/// budget for a live drift pass. After the budget is spent, every remaining
/// document is recorded unfetchable — the pass fails closed rather than
/// running unbounded.
const DRIFT_REQUEST_TIMEOUT_SECS: u64 = 15;
const DRIFT_TOTAL_BUDGET_SECS: u64 = 300;

/// Canonical refetch URL for a pinned upstream document — the same base the
/// authoring checker uses, so both roles classify the same bytes.
#[must_use]
pub fn upstream_url(repo: &str, commit: &str, upstream_path: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/modelcontextprotocol/{repo}/{commit}/{upstream_path}"
    )
}

fn load_source_manifest(root: &Path) -> Result<SourcesManifest, EaiError> {
    let manifest: SourcesManifest = load_json(root, ".agents/gmcp/sources.json")?;
    if manifest.schema != SOURCES_SCHEMA {
        return Err(EaiError::config(format!(
            "sources.json schema must be {SOURCES_SCHEMA}, got {:?}",
            manifest.schema
        )));
    }
    Ok(manifest)
}

fn pinned_commit<'a>(manifest: &'a SourcesManifest, file: &SourceFile) -> Option<&'a str> {
    manifest
        .upstream
        .get(&file.repo)
        .and_then(|pin| pin.get("commit"))
        .and_then(serde_json::Value::as_str)
}

fn compare_refetched(
    report: &mut DriftReport,
    file: &SourceFile,
    fetched: Result<Vec<u8>, String>,
) {
    report.checked += 1;
    match fetched {
        Err(detail) => report.findings.push(DriftFinding {
            document: file.document.clone(),
            kind: "unfetchable",
            detail,
        }),
        Ok(bytes) if bytes.len() as u64 == file.bytes && sha256_hex(&bytes) == file.sha256 => {
            report.matched += 1;
        }
        Ok(bytes) => report.findings.push(DriftFinding {
            document: file.document.clone(),
            kind: "hash_drift",
            detail: format!(
                "refetched {} bytes sha256 {}, pinned {} bytes sha256 {}",
                bytes.len(),
                sha256_hex(&bytes),
                file.bytes,
                file.sha256
            ),
        }),
    }
}

/// Deterministic drift check: compare a pre-fetched corpus directory —
/// keyed by the manifest's `document` name — against `sources.json`. A
/// missing file is an `unfetchable` finding, so the check fails closed on
/// anything it cannot classify.
pub fn check_drift_against_dir(root: &Path, refetched: &Path) -> Result<DriftReport, EaiError> {
    let manifest = load_source_manifest(root)?;
    let mut report = DriftReport {
        checked: 0,
        matched: 0,
        findings: Vec::new(),
    };
    for file in &manifest.files {
        confined_document(&file.document)?;
        let fetched = std::fs::read(refetched.join(&file.document))
            .map_err(|e| format!("refetched copy missing: {e}"));
        compare_refetched(&mut report, file, fetched);
    }
    Ok(report)
}

/// Live drift check: refetch every pinned upstream file and compare hashes
/// against `sources.json`. Network, HTTP, size-bound, and missing-pin
/// failures are all `unfetchable` findings — an unreachable upstream fails
/// closed rather than passing unverified. Bounded by
/// `DRIFT_REQUEST_TIMEOUT_SECS` per request and `DRIFT_TOTAL_BUDGET_SECS`
/// overall.
pub fn check_drift_remote(root: &Path) -> Result<DriftReport, EaiError> {
    let manifest = load_source_manifest(root)?;
    let mut report = DriftReport {
        checked: 0,
        matched: 0,
        findings: Vec::new(),
    };
    let started = std::time::Instant::now();
    let mut budget_spent = false;
    for file in &manifest.files {
        confined_document(&file.document)?;
        if budget_spent {
            compare_refetched(
                &mut report,
                file,
                Err(format!(
                    "drift time budget of {DRIFT_TOTAL_BUDGET_SECS}s already spent"
                )),
            );
            continue;
        }
        let fetched = (|| {
            let Some(commit) = pinned_commit(&manifest, file) else {
                return Err(format!("no pinned commit for repo {}", file.repo));
            };
            let url = upstream_url(&file.repo, commit, &file.upstream_path);
            let call = susi_http_transport::http_call(
                "GET",
                &url,
                &[("User-Agent", "susi-gmcp-drift")],
                DRIFT_REQUEST_TIMEOUT_SECS,
                5,
            )
            .map_err(|e| format!("fetch failed: {e}"))?;
            if call.status != 200 {
                return Err(format!("{url} -> HTTP {}", call.status));
            }
            call.into_bytes(MAX_ARTIFACT_BYTES)
                .map_err(|e| format!("read failed: {e}"))
        })();
        compare_refetched(&mut report, file, fetched);
        if started.elapsed().as_secs() >= DRIFT_TOTAL_BUDGET_SECS {
            budget_spent = true;
        }
    }
    Ok(report)
}

fn load_json<T: for<'de> Deserialize<'de>>(root: &Path, rel: &str) -> Result<T, EaiError> {
    let data = read(root, rel)?;
    serde_json::from_slice(&data).map_err(|e| EaiError::config(format!("parse {rel}: {e}")))
}

/// Verify the pinned ledger under `root` (the repository checkout).
///
/// Fail closed on: a missing or unparsable artifact, any vendored file whose
/// bytes or sha256 drifted from `sources.json`, a row that does not quote its
/// source verbatim, a duplicate row id, a missing trace field, or a required
/// revision/category with zero rows.
pub fn verify_pinned_ledger(root: &Path) -> Result<LedgerReport, EaiError> {
    let ledger: RequirementsLedger = load_json(root, ".agents/gmcp/requirements-ledger.json")?;
    if ledger.schema != LEDGER_SCHEMA {
        return Err(EaiError::config(format!(
            "requirements-ledger.json schema is {:?}, expected {LEDGER_SCHEMA:?}",
            ledger.schema
        )));
    }
    let manifest: SourcesManifest = load_json(root, ".agents/gmcp/sources.json")?;
    if manifest.schema != SOURCES_SCHEMA {
        return Err(EaiError::config(format!(
            "sources.json schema is {:?}, expected {SOURCES_SCHEMA:?}",
            manifest.schema
        )));
    }
    if manifest.revisions.modern != ledger.baseline.revisions.modern
        || manifest.revisions.compatibility != ledger.baseline.revisions.compatibility
    {
        return Err(EaiError::config(
            "ledger baseline revisions disagree with sources.json".to_string(),
        ));
    }
    if manifest.generated_unix == 0 {
        return Err(EaiError::config(
            "sources.json has no generation timestamp".to_string(),
        ));
    }
    let upstream = &manifest.upstream;
    for file in &manifest.files {
        if file.upstream_path.is_empty()
            || !upstream
                .get(&file.repo)
                .and_then(|pin| pin.get("commit"))
                .and_then(|commit| commit.as_str())
                .is_some_and(|commit| !commit.is_empty())
        {
            return Err(EaiError::config(format!(
                "{}: no pinned upstream commit — drift detection cannot fail closed",
                file.document
            )));
        }
    }
    for authority in &manifest.external_authorities {
        let named = authority.get("name").and_then(|v| v.as_str());
        let roled = authority.get("role").and_then(|v| v.as_str());
        if !named.is_some_and(|n| !n.is_empty()) || !roled.is_some_and(|r| !r.is_empty()) {
            return Err(EaiError::config(
                "an external authority lacks a name or role".to_string(),
            ));
        }
    }

    if ledger.requirements.len() > MAX_LEDGER_ROWS {
        return Err(EaiError::config(format!(
            "ledger carries {} rows, over the {MAX_LEDGER_ROWS}-row bound",
            ledger.requirements.len()
        )));
    }

    // document -> (absolute path, pinned sha256). Spec files live under
    // .agents/gmcp/spec/; repository documents (claim sources) at repo root.
    // Confinement is enforced before any join: a `..` or absolute document
    // name must not let verification read outside the pinned tree.
    let spec_root = root.join(".agents/gmcp/spec");
    let mut pinned: BTreeMap<String, (PathBuf, String)> = BTreeMap::new();
    for file in &manifest.files {
        confined_document(&file.document)?;
        if file.bytes > MAX_ARTIFACT_BYTES {
            return Err(EaiError::config(format!(
                "{} exceeds the {MAX_ARTIFACT_BYTES}-byte verification bound",
                file.document
            )));
        }
        let path = spec_root.join(&file.document);
        let on_disk = std::fs::metadata(&path)
            .map_err(|e| EaiError::io(format!("read vendored source {}: {e}", file.document)))?
            .len();
        // Cheap size check first: a drifted or oversized file is rejected
        // before hashing so verification stays bounded on hostile input.
        if on_disk != file.bytes {
            return Err(EaiError::config(format!(
                "vendored source {} drifted from its pinned hash",
                file.document
            )));
        }
        let data = std::fs::read(&path)
            .map_err(|e| EaiError::io(format!("read vendored source {}: {e}", file.document)))?;
        if sha256_hex(&data) != file.sha256 {
            return Err(EaiError::config(format!(
                "vendored source {} drifted from its pinned hash",
                file.document
            )));
        }
        pinned.insert(file.document.clone(), (path, file.sha256.clone()));
    }
    for doc in &manifest.repository_documents {
        confined_document(&doc.document)?;
        if doc.bytes > MAX_ARTIFACT_BYTES {
            return Err(EaiError::config(format!(
                "{} exceeds the {MAX_ARTIFACT_BYTES}-byte verification bound",
                doc.document
            )));
        }
        let path = root.join(&doc.document);
        let on_disk = std::fs::metadata(&path)
            .map_err(|e| EaiError::io(format!("read repository document {}: {e}", doc.document)))?
            .len();
        if on_disk != doc.bytes {
            return Err(EaiError::config(format!(
                "repository document {} drifted from its pinned hash",
                doc.document
            )));
        }
        let data = std::fs::read(&path)
            .map_err(|e| EaiError::io(format!("read repository document {}: {e}", doc.document)))?;
        if sha256_hex(&data) != doc.sha256 {
            return Err(EaiError::config(format!(
                "repository document {} drifted from its pinned hash",
                doc.document
            )));
        }
        pinned.insert(doc.document.clone(), (path, doc.sha256.clone()));
    }

    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for (document, (path, _)) in &pinned {
        let text = std::fs::read_to_string(path)
            .map_err(|e| EaiError::io(format!("read {document}: {e}")))?;
        texts.insert(document.clone(), text);
    }

    let mut ids: BTreeSet<&str> = BTreeSet::new();
    let mut revisions: BTreeMap<String, usize> = BTreeMap::new();
    let mut dispositions: BTreeMap<String, usize> = BTreeMap::new();
    let mut categories: BTreeMap<String, usize> = BTreeMap::new();
    let mut vectors: BTreeSet<String> = BTreeSet::new();

    for row in &ledger.requirements {
        if row.id.is_empty() || !ids.insert(row.id.as_str()) {
            return Err(EaiError::config(format!(
                "empty or duplicate requirement id {:?}",
                row.id
            )));
        }
        if row.clause.is_empty() {
            return Err(EaiError::config(format!("{}: empty clause", row.id)));
        }
        if row.categories.is_empty() {
            return Err(EaiError::config(format!("{}: no categories", row.id)));
        }
        if row.disposition.reason.is_empty() {
            return Err(EaiError::config(format!(
                "{}: empty disposition reason",
                row.id
            )));
        }
        let trace = &row.trace;
        for (name, value) in [
            ("vector", &trace.vector),
            ("owner", &trace.owner),
            ("production_path", &trace.production_path),
            ("positive_test", &trace.positive_test),
            ("negative_test", &trace.negative_test),
            ("evidence", &trace.evidence),
        ] {
            if value.is_empty() {
                return Err(EaiError::config(format!(
                    "{id}: empty trace.{name}",
                    id = row.id
                )));
            }
        }
        let Some(suffix) = trace.vector.strip_prefix("VC-203-") else {
            return Err(EaiError::config(format!("{}: bad vector", row.id)));
        };
        let vector_num: u32 = suffix
            .parse()
            .map_err(|_| EaiError::config(format!("{}: bad vector", row.id)))?;
        if !(1..=48).contains(&vector_num) {
            return Err(EaiError::config(format!("{}: bad vector", row.id)));
        }
        let expected_positive =
            format!("tests/gmcp_requirements.rs::gmcp_req_203_{suffix}_03_integration");
        let expected_negative =
            format!("tests/gmcp_requirements.rs::gmcp_req_203_{suffix}_04_adversarial");
        let expected_evidence = format!(".agents/gmcp/evidence/VC-203-{suffix}.jsonl");
        if trace.positive_test != expected_positive
            || trace.negative_test != expected_negative
            || trace.evidence != expected_evidence
        {
            return Err(EaiError::config(format!(
                "{}: trace does not follow the contract naming convention",
                row.id
            )));
        }

        let Some((_, sha)) = pinned.get(&row.source.document) else {
            return Err(EaiError::config(format!(
                "{}: unpinned source document {}",
                row.id, row.source.document
            )));
        };
        if &row.source.sha256 != sha {
            return Err(EaiError::config(format!(
                "{}: source pin mismatch for {}",
                row.id, row.source.document
            )));
        }
        let text = texts.get(&row.source.document).map_or("", String::as_str);
        if !text.contains(&row.clause) {
            return Err(EaiError::config(format!(
                "{}: clause is not verbatim in {}",
                row.id, row.source.document
            )));
        }

        *revisions.entry(row.revision.clone()).or_default() += 1;
        let disposition = match row.disposition.status {
            DispositionStatus::Modern => "modern",
            DispositionStatus::Adapter => "adapter",
            DispositionStatus::Optional => "optional",
        };
        *dispositions.entry(disposition.to_string()).or_default() += 1;
        for category in &row.categories {
            let name = match category {
                Category::Behavior => "behavior",
                Category::Error => "error",
                Category::Deprecation => "deprecation",
                Category::Security => "security",
                Category::Extension => "extension",
                Category::Registry => "registry",
                Category::Claim => "claim",
            };
            *categories.entry(name.to_string()).or_default() += 1;
        }
        vectors.insert(trace.vector.clone());
    }

    for revision in [
        manifest.revisions.compatibility.as_str(),
        manifest.revisions.modern.as_str(),
    ] {
        if !revisions.contains_key(revision) {
            return Err(EaiError::config(format!(
                "revision {revision} has no classified rows"
            )));
        }
    }
    for required in [
        "behavior",
        "error",
        "deprecation",
        "security",
        "extension",
        "claim",
    ] {
        if !categories.contains_key(required) {
            return Err(EaiError::config(format!(
                "category {required} has no classified rows"
            )));
        }
    }

    Ok(LedgerReport {
        rows: ledger.requirements.len(),
        documents: manifest.files.len(),
        revisions,
        dispositions,
        categories,
        vectors: vectors.len(),
    })
}
