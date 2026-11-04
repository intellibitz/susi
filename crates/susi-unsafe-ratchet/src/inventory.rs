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
    pub source_guards: Vec<ReviewedFile>,
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
        let mut guards = BTreeSet::new();
        for file in &component.source_guards {
            if !guards.insert(&file.path)
                || !measurement::valid_sha256(&file.sha256)
                || file.path.is_empty()
                || Path::new(&file.path)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("invalid or duplicate source guard".into());
            }
        }
    }
    Ok(())
}

pub fn read_baseline(path: &Path) -> Result<Baseline, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("missing baseline {}: {e}", path.display()))?;
    let baseline: Baseline = crate::strict_json::parse(&bytes).map_err(|e| {
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
    let historical: UnsafeBaseline = crate::strict_json::parse(baseline.historical_json.as_bytes())
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
        // Reviewed boundaries can live below the measured crate entrypoint.
        // A stable root declaration must not approve a changed implementation.
        let mut reviewed_source_matches = true;
        if let Some(review) = baseline
            .review
            .components
            .iter()
            .find(|r| r.identity == package.identity)
        {
            for file in &review.reviewed_files {
                let path = measurement::contained_file(&context.directory, &file.path)?;
                reviewed_source_matches &= measurement::file_sha256(&path)? == file.sha256;
            }
            for file in &review.source_guards {
                let path =
                    measurement::contained_file(Path::new(&measured.source_root), &file.path)?;
                reviewed_source_matches &= measurement::file_sha256(&path)? == file.sha256;
            }
        }
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
        }) || !reviewed_source_matches
        {
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
    let historical: UnsafeBaseline = crate::strict_json::parse(&bytes)?;
    let base = measurement::read_measurement(base_path)?;
    let candidate = measurement::read_measurement(candidate_path)?;
    let required = migration_reviews(&historical, &base, &candidate)?;
    let review: Review =
        crate::strict_json::parse(&std::fs::read(review_path).map_err(|e| e.to_string())?)?;
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
        for file in &component.source_guards {
            let path = measurement::contained_file(Path::new(&candidate.source_root), &file.path)?;
            if measurement::file_sha256(&path)? != file.sha256 {
                return Err(format!("review source guard changed: {}", path.display()));
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::measurement::fixture::{self, dep_identity, REGISTRY};
    use std::path::PathBuf;

    struct Env {
        dir: PathBuf,
        historical: PathBuf,
        base: PathBuf,
        candidate_path: PathBuf,
        review: PathBuf,
        candidate: Measurement,
    }

    impl Env {
        fn dep_source(&self) -> PathBuf {
            self.dir.join("reg").join("dep").join("src").join("lib.rs")
        }
    }

    fn historical() -> UnsafeBaseline {
        UnsafeBaseline {
            features: "default".into(),
            exposure: vec![UnsafeExposure {
                crate_name: "dep".into(),
                unsafe_fns: 1,
                first_party_exception: false,
            }],
            sbom: vec![SbomEntry {
                crate_name: "dep".into(),
                version: "1.0.0".into(),
                provenance: REGISTRY.into(),
            }],
            recorded_unix: 1,
        }
    }

    /// Writes the historical inventory plus matched base/candidate scans. The
    /// candidate's `dep` entry point carries `candidate_hash`.
    fn setup(tag: &str, candidate_hash: char) -> Env {
        let dir = fixture::scratch(tag);
        std::fs::create_dir_all(dir.join("reg").join("dep").join("src")).unwrap();
        let env = Env {
            historical: dir.join("historical.json"),
            base: dir.join("base.json"),
            candidate_path: dir.join("candidate.json"),
            review: dir.join("review.json"),
            candidate: fixture::measurement(&dir, 'b', true, candidate_hash),
            dir,
        };
        std::fs::write(env.dep_source(), "// reviewed boundary").unwrap();
        measurement::write_json_atomic(&env.historical, &historical()).unwrap();
        measurement::write_json_atomic(&env.base, &fixture::measurement(&env.dir, 'a', true, 'a'))
            .unwrap();
        measurement::write_json_atomic(&env.candidate_path, &env.candidate).unwrap();
        env
    }

    fn dep_component(env: &Env) -> ComponentReview {
        ComponentReview {
            identity: dep_identity(),
            decision: "accept-forbid-exposure".into(),
            risk: "dependency exposes crate-level unsafe code".into(),
            boundary: "confined to the dep crate entry point".into(),
            justification: "reviewed source hash pinned in this baseline".into(),
            reviewed_files: vec![ReviewedFile {
                path: "src/lib.rs".into(),
                sha256: measurement::file_sha256(&env.dep_source()).unwrap(),
            }],
            source_guards: vec![],
        }
    }

    fn bound_review(env: &Env, components: Vec<ComponentReview>) -> Review {
        Review {
            schema: "susi/unsafe-component-review/v1".into(),
            reviewer: "tester".into(),
            historical_baseline_sha256: measurement::file_sha256(&env.historical).unwrap(),
            base_measurement_sha256: measurement::file_sha256(&env.base).unwrap(),
            candidate_measurement_sha256: measurement::file_sha256(&env.candidate_path).unwrap(),
            scanner_sha256: env.candidate.scanner_sha256.clone(),
            components,
        }
    }

    fn migrate_with(env: &Env, review: &Review) -> Result<Baseline, String> {
        measurement::write_json_atomic(&env.review, review).unwrap();
        migrate(&env.historical, &env.base, &env.candidate_path, &env.review)
    }

    fn unbound_review() -> Review {
        Review {
            schema: "susi/unsafe-component-review/v1".into(),
            reviewer: "tester".into(),
            historical_baseline_sha256: "a".repeat(64),
            base_measurement_sha256: "b".repeat(64),
            candidate_measurement_sha256: "c".repeat(64),
            scanner_sha256: "d".repeat(64),
            components: vec![ComponentReview {
                identity: dep_identity(),
                decision: "accept-forbid-exposure".into(),
                risk: "dependency exposes crate-level unsafe code".into(),
                boundary: "confined to the dep crate entry point".into(),
                justification: "reviewed source hash pinned in this baseline".into(),
                reviewed_files: vec![ReviewedFile {
                    path: "src/lib.rs".into(),
                    sha256: "e".repeat(64),
                }],
                source_guards: vec![ReviewedFile {
                    path: "guard.rs".into(),
                    sha256: "f".repeat(64),
                }],
            }],
        }
    }

    #[test]
    fn files_digest_ignores_order_but_binds_content() {
        let root = fixture::scratch("digest");
        let mut package = fixture::measurement(&root, 'a', true, 'a').packages[1].clone();
        let mut second = package.files[0].clone();
        second.path = "src/extra.rs".into();
        second.sha256 = "c".repeat(64);
        package.files.push(second);
        let forward = files_digest(&package).unwrap();
        package.files.reverse();
        assert_eq!(files_digest(&package).unwrap(), forward);
        package.files[0].sha256 = "d".repeat(64);
        assert_ne!(files_digest(&package).unwrap(), forward);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn units_counts_only_packages_without_a_forbid_declaration() {
        let root = fixture::scratch("units");
        let unsafe_dep = fixture::measurement(&root, 'a', true, 'a');
        let forbidding = fixture::measurement(&root, 'a', false, 'a');
        assert_eq!(units(&unsafe_dep.packages[1]), 1);
        assert_eq!(units(&forbidding.packages[1]), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn validate_identity_requires_name_version_and_source() {
        assert!(validate_identity(&dep_identity()).is_ok());
        for field in 0..3 {
            let mut identity = dep_identity();
            match field {
                0 => identity.name.clear(),
                1 => identity.version.clear(),
                _ => identity.source.clear(),
            }
            assert!(validate_identity(&identity).is_err(), "field {field}");
        }
    }

    #[test]
    fn validate_review_accepts_a_complete_review_and_rejects_each_defect() {
        fn rejects(needle: &str, mutate: impl FnOnce(&mut Review)) {
            let mut review = unbound_review();
            mutate(&mut review);
            let err = validate_review(&review).unwrap_err();
            assert!(err.contains(needle), "wanted {needle:?}, got {err}");
        }
        assert!(validate_review(&unbound_review()).is_ok());
        rejects("missing component review identity/schema", |r| {
            r.schema = "other".into();
        });
        rejects("missing component review identity/schema", |r| {
            r.reviewer = "  ".into();
        });
        rejects("invalid review binding digest", |r| {
            r.scanner_sha256 = "XYZ".into();
        });
        rejects("incomplete package identity", |r| {
            r.components[0].identity.name.clear();
        });
        rejects("duplicate or unaccepted component review", |r| {
            r.components[0].decision = "reject".into();
        });
        rejects("duplicate or unaccepted component review", |r| {
            let copy = r.components[0].clone();
            r.components.push(copy);
        });
        rejects("lacks a substantive rationale", |r| {
            r.components[0].risk = "too short".into();
        });
        rejects("no source evidence", |r| {
            r.components[0].reviewed_files.clear();
        });
        rejects("invalid or duplicate reviewed source file", |r| {
            r.components[0].reviewed_files[0].path = "../escape.rs".into();
        });
        rejects("invalid or duplicate reviewed source file", |r| {
            let copy = r.components[0].reviewed_files[0].clone();
            r.components[0].reviewed_files.push(copy);
        });
        rejects("invalid or duplicate reviewed source file", |r| {
            r.components[0].reviewed_files[0].sha256 = "short".into();
        });
        rejects("invalid or duplicate source guard", |r| {
            r.components[0].source_guards[0].path = String::new();
        });
    }

    #[test]
    fn migration_reviews_requires_review_only_for_new_or_changed_exposure() {
        let root = fixture::scratch("migration");
        let base = fixture::measurement(&root, 'a', true, 'a');
        let dep_only = BTreeSet::from([dep_identity()]);

        // Identical entry points under an accepted historical exposure: carried forward.
        let same = fixture::measurement(&root, 'b', true, 'a');
        assert!(migration_reviews(&historical(), &base, &same)
            .unwrap()
            .is_empty());

        // A changed entry point needs a component review.
        let changed = fixture::measurement(&root, 'b', true, 'c');
        assert_eq!(
            migration_reviews(&historical(), &base, &changed).unwrap(),
            dep_only
        );

        // A crate that forbids unsafe never needs review, even when it changed.
        let forbidding = fixture::measurement(&root, 'b', false, 'c');
        assert!(migration_reviews(&historical(), &base, &forbidding)
            .unwrap()
            .is_empty());

        // Exposure absent from, or version-drifted against, the historical inventory.
        let mut other_crate = historical();
        other_crate.exposure[0].crate_name = "other".into();
        assert_eq!(
            migration_reviews(&other_crate, &base, &same).unwrap(),
            dep_only
        );
        let mut first_party = historical();
        first_party.exposure[0].first_party_exception = true;
        assert_eq!(
            migration_reviews(&first_party, &base, &same).unwrap(),
            dep_only
        );
        let mut drifted = historical();
        drifted.sbom[0].version = "0.9.0".into();
        assert_eq!(migration_reviews(&drifted, &base, &same).unwrap(), dep_only);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_reviews_refuses_unmatched_scans_and_empty_history() {
        let root = fixture::scratch("migration-bad");
        let base = fixture::measurement(&root, 'a', true, 'a');
        let same = fixture::measurement(&root, 'b', true, 'a');
        let needle = "migration requires historical inventory";

        let mut empty_exposure = historical();
        empty_exposure.exposure.clear();
        let mut empty_sbom = historical();
        empty_sbom.sbom.clear();
        let mut no_time = historical();
        no_time.recorded_unix = 0;
        let mut wrong_features = historical();
        wrong_features.features = "cuda".into();
        for history in [empty_exposure, empty_sbom, no_time, wrong_features] {
            let err = migration_reviews(&history, &base, &same).unwrap_err();
            assert!(err.contains(needle), "{err}");
        }

        let same_commit = fixture::measurement(&root, 'a', true, 'a');
        assert!(migration_reviews(&historical(), &base, &same_commit).is_err());

        let mut other_scanner = same.clone();
        other_scanner.scanner_sha256 = "9".repeat(64);
        assert!(migration_reviews(&historical(), &base, &other_scanner).is_err());
        let mut other_cargo = same.clone();
        other_cargo.cargo_version = "cargo 2.0.0".into();
        assert!(migration_reviews(&historical(), &base, &other_cargo).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_carry_forward_migrates_validates_and_issues_a_receipt() {
        let env = setup("clean", 'a');
        let baseline = migrate_with(&env, &bound_review(&env, vec![])).unwrap();
        assert_eq!(baseline.schema, BASELINE_SCHEMA);
        assert_eq!(baseline.allowances.len(), 2);
        let basis = |name: &str| {
            baseline
                .allowances
                .iter()
                .find(|a| a.identity.name == name)
                .unwrap()
                .basis
                .clone()
        };
        assert_eq!(basis("app"), "workspace-member");
        assert_eq!(basis("dep"), "matched-historical-exposure");
        assert!(check_inventory(&env.candidate, &baseline)
            .unwrap()
            .is_empty());

        // The baseline survives a write/read round trip under strict parsing.
        let path = env.dir.join("baseline.json");
        measurement::write_json_atomic(&path, &baseline).unwrap();
        let read = read_baseline(&path).unwrap();
        assert_eq!(read.review_sha256, baseline.review_sha256);

        let issued = receipt(&env.candidate, &env.candidate_path, &read, &path).unwrap();
        assert_eq!(issued.verdict, "clean");
        assert!(issued.complete);
        assert_eq!(issued.measured_packages, 2);
        assert_eq!(issued.measured_files, 2);
        assert!(issued.unreviewed_exposures.is_empty());
        assert_eq!(issued.review_sha256, read.review_sha256);
        assert_eq!(
            issued.measurement_sha256,
            measurement::file_sha256(&env.candidate_path).unwrap()
        );

        // Changed dependency source is unreviewed exposure and blocks the receipt.
        let mut changed = env.candidate.clone();
        changed.packages[1].files[0].sha256 = "c".repeat(64);
        assert_eq!(
            check_inventory(&changed, &read).unwrap(),
            vec![dep_identity()]
        );
        let err = receipt(&changed, &env.candidate_path, &read, &path).unwrap_err();
        assert!(err.contains("requires review"), "{err}");
        assert!(err.contains("dep@1.0.0"), "{err}");

        // A measurement from another context is not comparable at all.
        let mut moved = env.candidate.clone();
        moved.target = "aarch64-pc-windows-msvc".into();
        assert!(check_inventory(&moved, &read).is_err());

        let _ = std::fs::remove_dir_all(&env.dir);
    }

    #[test]
    fn changed_dependency_needs_a_bound_review_and_source_drift_revokes_it() {
        let env = setup("review", 'b');

        let err = migrate_with(&env, &bound_review(&env, vec![])).unwrap_err();
        assert!(err.contains("component review set differs"), "{err}");

        let baseline = migrate_with(&env, &bound_review(&env, vec![dep_component(&env)])).unwrap();
        let dep = baseline
            .allowances
            .iter()
            .find(|a| a.identity == dep_identity())
            .unwrap();
        assert_eq!(dep.basis, "component-review");
        assert!(check_inventory(&env.candidate, &baseline)
            .unwrap()
            .is_empty());

        // Editing the reviewed boundary after sign-off makes the exposure unreviewed again.
        std::fs::write(env.dep_source(), "// drifted after review").unwrap();
        assert_eq!(
            check_inventory(&env.candidate, &baseline).unwrap(),
            vec![dep_identity()]
        );
        let _ = std::fs::remove_dir_all(&env.dir);
    }

    #[test]
    fn migrate_refuses_reviews_that_do_not_bind_the_exact_evidence() {
        let env = setup("unbound", 'b');

        let mut review = bound_review(&env, vec![dep_component(&env)]);
        review.historical_baseline_sha256 = "d".repeat(64);
        let err = migrate_with(&env, &review).unwrap_err();
        assert!(
            err.contains("does not bind the exact matched scans"),
            "{err}"
        );

        let mut review = bound_review(&env, vec![dep_component(&env)]);
        review.scanner_sha256 = "d".repeat(64);
        assert!(migrate_with(&env, &review).is_err());

        let mut component = dep_component(&env);
        component.reviewed_files[0].sha256 = "d".repeat(64);
        let err = migrate_with(&env, &bound_review(&env, vec![component])).unwrap_err();
        assert!(err.contains("review source changed"), "{err}");

        let mut component = dep_component(&env);
        component.source_guards.push(ReviewedFile {
            path: "absent-guard.rs".into(),
            sha256: "d".repeat(64),
        });
        assert!(migrate_with(&env, &bound_review(&env, vec![component])).is_err());
        let _ = std::fs::remove_dir_all(&env.dir);
    }

    #[test]
    fn validate_baseline_rejects_tampered_proofs() {
        let env = setup("tamper", 'b');
        let good = migrate_with(&env, &bound_review(&env, vec![dep_component(&env)])).unwrap();
        assert!(validate_baseline(&good).is_ok());

        fn rejects(good: &Baseline, needle: &str, mutate: impl FnOnce(&mut Baseline)) {
            let mut tampered = good.clone();
            mutate(&mut tampered);
            let err = validate_baseline(&tampered).unwrap_err();
            assert!(err.contains(needle), "wanted {needle:?}, got {err}");
        }
        let schema = "invalid baseline schema/unit/context";
        rejects(&good, schema, |b| {
            b.schema = "susi/unsafe-baseline/v1".into()
        });
        rejects(&good, schema, |b| b.features = "cuda".into());
        rejects(&good, schema, |b| b.allowances.clear());
        rejects(&good, schema, |b| b.recorded_unix = 0);
        rejects(&good, "missing component review identity/schema", |b| {
            b.review.reviewer.clear();
        });
        rejects(&good, "baseline review binding mismatch", |b| {
            b.review.components[0].risk = "a different but still long rationale".into();
        });
        rejects(&good, "baseline matched-scan proof mismatch", |b| {
            b.base_source_commit = "0".repeat(40);
        });
        rejects(
            &good,
            "allowance differs from measured candidate proof",
            |b| {
                b.allowances[1].unsafe_units = 0;
            },
        );
        rejects(&good, "unsupported baseline allowance basis", |b| {
            b.allowances[1].basis = "trust-me".into();
        });
        rejects(&good, "legacy baseline projection differs", |b| {
            b.exposure.clear();
        });
        let _ = std::fs::remove_dir_all(&env.dir);
    }

    #[test]
    fn read_baseline_reports_missing_and_pre_v2_files() {
        let dir = fixture::scratch("read-baseline");
        let err = read_baseline(&dir.join("absent.json")).unwrap_err();
        assert!(err.contains("missing baseline"), "{err}");

        let legacy = dir.join("legacy.json");
        std::fs::write(&legacy, r#"{"features": "default", "exposure": []}"#).unwrap();
        let err = read_baseline(&legacy).unwrap_err();
        assert!(
            err.contains("needs complete reviewed v2 migration"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
