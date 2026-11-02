//! Full-identity inventory admission and evidence-bound baseline migration.
use crate::measurement::{self, Identity, Measurement, PackageMeasurement};
use crate::{SbomEntry, UnsafeBaseline, UnsafeExposure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const BASELINE_SCHEMA: &str = "susi/unsafe-baseline/v2";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentReview {
    pub identity: Identity,
    pub decision: String,
    pub risk: String,
    pub boundary: String,
    pub justification: String,
    pub reviewed_files: Vec<ReviewedFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub schema: String,
    pub reviewer: String,
    pub historical_baseline_sha256: String,
    pub base_measurement_sha256: String,
    pub candidate_measurement_sha256: String,
    pub scanner_sha256: String,
    pub components: Vec<ComponentReview>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowance {
    pub identity: Identity,
    pub first_party: bool,
    pub unsafe_units: u8,
    pub files_sha256: String,
    pub features: Vec<String>,
    pub dependencies: Vec<Identity>,
    pub basis: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub schema: String,
    // The historical projection remains readable by v1 callers. Admission
    // uses only the complete full-identity allowances below.
    pub features: String,
    pub exposure: Vec<UnsafeExposure>,
    pub sbom: Vec<SbomEntry>,
    pub recorded_unix: u64,
    pub unit: String,
    pub target: String,
    pub historical_baseline_sha256: String,
    pub base_source_commit: String,
    pub candidate_source_commit: String,
    pub base_measurement_sha256: String,
    pub candidate_measurement_sha256: String,
    pub review_sha256: String,
    pub review: Review,
    pub allowances: Vec<Allowance>,
    pub historical_json: String,
    pub base_measurement: Measurement,
    pub candidate_measurement: Measurement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub complete: bool,
    pub verdict: String,
    pub source_commit: String,
    pub source_tree: String,
    pub cargo_lock_sha256: String,
    pub target: String,
    pub features: String,
    pub unit: String,
    pub measurement_sha256: String,
    pub baseline_sha256: String,
    pub scanner_sha256: String,
    pub comparator_sha256: String,
    pub graph_sha256: String,
    pub measured_packages: usize,
    pub measured_files: usize,
    pub unreviewed_exposures: Vec<Identity>,
    pub review_sha256: String,
}

fn units(package: &PackageMeasurement) -> u8 {
    u8::from(!package.forbids_unsafe)
}

pub fn files_digest(package: &PackageMeasurement) -> Result<String, String> {
    let mut files = package.files.clone();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    measurement::json_sha256(&files)
}

fn validate_identity(identity: &Identity) -> Result<(), String> {
    if identity.name.is_empty() || identity.version.is_empty() || identity.source.is_empty() {
        return Err("incomplete package identity in baseline/review".into());
    }
    Ok(())
}

fn validate_review(review: &Review) -> Result<(), String> {
    if review.schema != "susi/unsafe-component-review/v1" || review.reviewer.trim().is_empty() {
        return Err("missing component review identity/schema".into());
    }
    for hash in [
        &review.historical_baseline_sha256,
        &review.base_measurement_sha256,
        &review.candidate_measurement_sha256,
        &review.scanner_sha256,
    ] {
        if !measurement::valid_sha256(hash) {
            return Err("invalid review binding digest".into());
        }
    }
    let mut identities = BTreeSet::new();
    for component in &review.components {
        validate_identity(&component.identity)?;
        if !identities.insert(&component.identity) || component.decision != "accept-forbid-exposure"
        {
            return Err("duplicate or unaccepted component review".into());
        }
        for rationale in [
            &component.risk,
            &component.boundary,
            &component.justification,
        ] {
            if rationale.trim().len() < 24 {
                return Err("component review lacks a substantive rationale".into());
            }
        }
        let mut paths = BTreeSet::new();
        for file in &component.reviewed_files {
            if !paths.insert(&file.path)
                || !measurement::valid_sha256(&file.sha256)
                || file.path.is_empty()
                || Path::new(&file.path)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("invalid or duplicate reviewed source file".into());
            }
        }
        if paths.is_empty() {
            return Err("component review has no source evidence".into());
        }
    }
    Ok(())
}

pub fn read_baseline(path: &Path) -> Result<Baseline, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("missing baseline {}: {e}", path.display()))?;
    let baseline: Baseline = serde_json::from_slice(&bytes).map_err(|e| {
        format!("baseline needs complete reviewed v2 migration; original retained: {e}")
    })?;
    validate_baseline(&baseline)?;
    Ok(baseline)
}

pub fn validate_baseline(baseline: &Baseline) -> Result<(), String> {
    if baseline.schema != BASELINE_SCHEMA
        || baseline.unit != measurement::UNIT
        || baseline.features != "default"
        || baseline.target.is_empty()
        || baseline.allowances.is_empty()
        || baseline.recorded_unix == 0
    {
        return Err("invalid baseline schema/unit/context or empty inventory".into());
    }
    validate_review(&baseline.review)?;
    let historical: UnsafeBaseline = serde_json::from_str(&baseline.historical_json)
        .map_err(|e| format!("invalid historical inventory proof: {e}"))?;
    let required = migration_reviews(
        &historical,
        &baseline.base_measurement,
        &baseline.candidate_measurement,
    )?;
    let candidate = &baseline.candidate_measurement;
    let selected = measurement::validate(candidate)?;
    let exposure: Vec<_> = candidate
        .packages
        .iter()
        .filter(|p| units(p) != 0)
        .map(|p| UnsafeExposure {
            crate_name: p.identity.name.clone(),
            unsafe_fns: u32::from(units(p)),
            first_party_exception: p.first_party,
        })
        .collect();
    let sbom: Vec<_> = candidate
        .packages
        .iter()
        .map(|p| SbomEntry {
            crate_name: p.identity.name.clone(),
            version: p.identity.version.clone(),
            provenance: p.identity.source.clone(),
        })
        .collect();
    if baseline.exposure != exposure || baseline.sbom != sbom {
        return Err("legacy baseline projection differs from complete candidate proof".into());
    }
    if measurement::sha256(baseline.historical_json.as_bytes())
        != baseline.historical_baseline_sha256
        || measurement::artifact_sha256(&baseline.base_measurement)?
            != baseline.base_measurement_sha256
        || measurement::artifact_sha256(candidate)? != baseline.candidate_measurement_sha256
        || baseline.base_source_commit != baseline.base_measurement.source_commit
        || baseline.candidate_source_commit != candidate.source_commit
        || baseline.review.scanner_sha256 != candidate.scanner_sha256
        || baseline.target != candidate.target
        || baseline.features != candidate.features
        || baseline.unit != candidate.unit
        || baseline.allowances.len() != candidate.packages.len()
    {
        return Err("baseline matched-scan proof mismatch".into());
    }
    if measurement::json_sha256(&baseline.review)? != baseline.review_sha256
        || baseline.historical_baseline_sha256 != baseline.review.historical_baseline_sha256
        || baseline.base_measurement_sha256 != baseline.review.base_measurement_sha256
        || baseline.candidate_measurement_sha256 != baseline.review.candidate_measurement_sha256
    {
        return Err("baseline review binding mismatch".into());
    }
    for revision in [
        &baseline.base_source_commit,
        &baseline.candidate_source_commit,
    ] {
        if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid baseline source revision".into());
        }
    }
    let mut identities = BTreeSet::new();
    let reviewed: BTreeSet<_> = baseline
        .review
        .components
        .iter()
        .map(|c| &c.identity)
        .collect();
    if required != reviewed.iter().map(|id| (*id).clone()).collect() {
        return Err("baseline component review coverage differs from matched-scan proof".into());
    }
    for allowance in &baseline.allowances {
        validate_identity(&allowance.identity)?;
        let package = candidate
            .packages
            .iter()
            .find(|p| p.identity == allowance.identity)
            .ok_or("allowance absent from candidate proof")?;
        let context = selected
            .iter()
            .find(|p| p.identity == package.identity)
            .ok_or("allowance graph context missing")?;
        if allowance.first_party != package.first_party
            || allowance.unsafe_units != units(package)
            || allowance.files_sha256 != files_digest(package)?
            || allowance.features != context.features
            || allowance.dependencies != context.dependencies
        {
            return Err("allowance differs from measured candidate proof".into());
        }
        if !identities.insert(&allowance.identity)
            || allowance.unsafe_units > 1
            || !measurement::valid_sha256(&allowance.files_sha256)
        {
            return Err("invalid or duplicate baseline allowance".into());
        }
        match allowance.basis.as_str() {
            "workspace-member" if allowance.first_party => {}
            "forbids-unsafe" if !allowance.first_party && allowance.unsafe_units == 0 => {}
            "matched-historical-exposure"
                if !allowance.first_party
                    && allowance.unsafe_units == 1
                    && !required.contains(&allowance.identity) => {}
            "component-review"
                if !allowance.first_party
                    && allowance.unsafe_units == 1
                    && reviewed.contains(&allowance.identity) => {}
            _ => return Err("unsupported baseline allowance basis".into()),
        }
    }
    if reviewed.iter().any(|id| {
        !baseline
            .allowances
            .iter()
            .any(|a| &a.identity == *id && a.basis == "component-review")
    }) {
        return Err("review component is not a reviewed allowance".into());
    }
    Ok(())
}

pub fn check_inventory(
    measured: &Measurement,
    baseline: &Baseline,
) -> Result<Vec<Identity>, String> {
    let selected = measurement::validate(measured)?;
    validate_baseline(baseline)?;
    if measured.target != baseline.target
        || measured.features != baseline.features
        || measured.unit != baseline.unit
    {
        return Err("baseline and measurement contexts differ".into());
    }
    let mut unreviewed = Vec::new();
    for package in &measured.packages {
        if package.first_party || units(package) == 0 {
            continue;
        }
        let hash = files_digest(package)?;
        let context = selected
            .iter()
            .find(|p| p.identity == package.identity)
            .ok_or("measured graph context missing")?;
        if !baseline.allowances.iter().any(|a| {
            a.identity == package.identity
                && !a.first_party
                && a.unsafe_units >= units(package)
                && a.files_sha256 == hash
                && context.features.iter().all(|f| a.features.contains(f))
                && context
                    .dependencies
                    .iter()
                    .all(|d| a.dependencies.contains(d))
        }) {
            unreviewed.push(package.identity.clone());
        }
    }
    unreviewed.sort();
    Ok(unreviewed)
}

pub fn receipt(
    measured: &Measurement,
    measurement_path: &Path,
    baseline: &Baseline,
    baseline_path: &Path,
) -> Result<Receipt, String> {
    let unreviewed = check_inventory(measured, baseline)?;
    if !unreviewed.is_empty() {
        return Err(format!(
            "new or changed unsafe exposure requires review: {}",
            unreviewed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Receipt {
        schema: "susi/unsafe-ratchet/v2".into(),
        complete: true,
        verdict: "clean".into(),
        source_commit: measured.source_commit.clone(),
        source_tree: measured.source_tree.clone(),
        cargo_lock_sha256: measured.cargo_lock_sha256.clone(),
        target: measured.target.clone(),
        features: measured.features.clone(),
        unit: measured.unit.clone(),
        measurement_sha256: measurement::file_sha256(measurement_path)?,
        baseline_sha256: measurement::file_sha256(baseline_path)?,
        scanner_sha256: measured.scanner_sha256.clone(),
        comparator_sha256: measurement::file_sha256(
            &std::env::current_exe().map_err(|e| e.to_string())?,
        )?,
        graph_sha256: measured.graph_sha256.clone(),
        measured_packages: measured.packages.len(),
        measured_files: measured.packages.iter().map(|p| p.files.len()).sum(),
        unreviewed_exposures: unreviewed,
        review_sha256: baseline.review_sha256.clone(),
    })
}

/// Carry-forward requires unchanged full identity and source measurement in
/// distinct clean scans, plus the historical baseline's accepted exposure.
pub fn migration_reviews(
    historical: &UnsafeBaseline,
    base: &Measurement,
    candidate: &Measurement,
) -> Result<BTreeSet<Identity>, String> {
    let base_graph = measurement::validate(base)?;
    let candidate_graph = measurement::validate(candidate)?;
    if base.target != candidate.target
        || base.features != candidate.features
        || base.unit != candidate.unit
        || base.scanner_sha256 != candidate.scanner_sha256
        || base.scanner_version != candidate.scanner_version
        || base.cargo_version != candidate.cargo_version
        || historical.features != candidate.features
        || historical.exposure.is_empty()
        || historical.sbom.is_empty()
        || historical.recorded_unix == 0
        || base.source_commit == candidate.source_commit
    {
        return Err("migration requires historical inventory and distinct complete scans from the same scanner/context".into());
    }
    let mut required = BTreeSet::new();
    for package in &candidate.packages {
        if package.first_party || units(package) == 0 {
            continue;
        }
        let prior = base
            .packages
            .iter()
            .find(|p| p.identity == package.identity && !p.first_party);
        let same_files = match prior {
            Some(prior) => files_digest(prior)? == files_digest(package)?,
            None => false,
        };
        let context = candidate_graph
            .iter()
            .find(|p| p.identity == package.identity)
            .ok_or("candidate graph context missing")?;
        let same_context = base_graph
            .iter()
            .find(|p| p.identity == package.identity)
            .is_some_and(|prior| {
                context.features.iter().all(|f| prior.features.contains(f))
                    && context
                        .dependencies
                        .iter()
                        .all(|d| prior.dependencies.contains(d))
            });
        let accepted = historical.exposure.iter().any(|e| {
            e.crate_name == package.identity.name
                && !e.first_party_exception
                && e.unsafe_fns >= u32::from(units(package))
        }) && historical.sbom.iter().any(|e| {
            e.crate_name == package.identity.name
                && e.version == package.identity.version
                && e.provenance == package.identity.source
        });
        if !same_files || !same_context || !accepted {
            required.insert(package.identity.clone());
        }
    }
    Ok(required)
}

pub fn migrate(
    historical_path: &Path,
    base_path: &Path,
    candidate_path: &Path,
    review_path: &Path,
) -> Result<Baseline, String> {
    let bytes = std::fs::read(historical_path).map_err(|e| e.to_string())?;
    let historical: UnsafeBaseline = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let base = measurement::read_measurement(base_path)?;
    let candidate = measurement::read_measurement(candidate_path)?;
    let required = migration_reviews(&historical, &base, &candidate)?;
    let review: Review =
        serde_json::from_slice(&std::fs::read(review_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    validate_review(&review)?;
    if review.historical_baseline_sha256 != measurement::sha256(&bytes)
        || review.base_measurement_sha256 != measurement::file_sha256(base_path)?
        || review.candidate_measurement_sha256 != measurement::file_sha256(candidate_path)?
        || review.scanner_sha256 != candidate.scanner_sha256
    {
        return Err("review does not bind the exact matched scans and historical inventory".into());
    }
    let reviewed: BTreeSet<_> = review
        .components
        .iter()
        .map(|c| c.identity.clone())
        .collect();
    if required != reviewed {
        return Err(format!(
            "component review set differs: required {required:?}; supplied {reviewed:?}"
        ));
    }
    let expected = measurement::validate(&candidate)?;
    for component in &review.components {
        let package = expected
            .iter()
            .find(|p| p.identity == component.identity)
            .ok_or("review package not selected")?;
        for file in &component.reviewed_files {
            let path = measurement::contained_file(&package.directory, &file.path)?;
            if measurement::file_sha256(&path)? != file.sha256 {
                return Err(format!("review source changed: {}", path.display()));
            }
        }
    }
    let mut allowances = Vec::new();
    for package in &candidate.packages {
        let context = expected
            .iter()
            .find(|p| p.identity == package.identity)
            .ok_or("candidate graph context missing")?;
        let basis = if package.first_party {
            "workspace-member"
        } else if units(package) == 0 {
            "forbids-unsafe"
        } else if required.contains(&package.identity) {
            "component-review"
        } else {
            "matched-historical-exposure"
        };
        allowances.push(Allowance {
            identity: package.identity.clone(),
            first_party: package.first_party,
            unsafe_units: units(package),
            files_sha256: files_digest(package)?,
            features: context.features.clone(),
            dependencies: context.dependencies.clone(),
            basis: basis.into(),
        });
    }
    let baseline = Baseline {
        schema: BASELINE_SCHEMA.into(),
        features: candidate.features.clone(),
        exposure: candidate
            .packages
            .iter()
            .filter(|p| units(p) != 0)
            .map(|p| UnsafeExposure {
                crate_name: p.identity.name.clone(),
                unsafe_fns: u32::from(units(p)),
                first_party_exception: p.first_party,
            })
            .collect(),
        sbom: candidate
            .packages
            .iter()
            .map(|p| SbomEntry {
                crate_name: p.identity.name.clone(),
                version: p.identity.version.clone(),
                provenance: p.identity.source.clone(),
            })
            .collect(),
        recorded_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs(),
        unit: candidate.unit.clone(),
        target: candidate.target.clone(),
        historical_baseline_sha256: measurement::sha256(&bytes),
        base_source_commit: base.source_commit.clone(),
        candidate_source_commit: candidate.source_commit.clone(),
        base_measurement_sha256: review.base_measurement_sha256.clone(),
        candidate_measurement_sha256: review.candidate_measurement_sha256.clone(),
        review_sha256: measurement::json_sha256(&review)?,
        review,
        allowances,
        historical_json: String::from_utf8(bytes).map_err(|e| e.to_string())?,
        base_measurement: base,
        candidate_measurement: candidate,
    };
    validate_baseline(&baseline)?;
    Ok(baseline)
}
