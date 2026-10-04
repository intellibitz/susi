//! `unsafe_ratchet` — the production gate behind the unsafe-exposure
//! ratchet (VC-201-097). Consumes a `cargo geiger --output-format Json`
//! report plus `cargo metadata --locked` output, builds the current
//! [`UnsafeExposure`]/[`SbomEntry`] sets, and asks [`ratchet_unsafe`]
//! whether any new unsafe exposure needs review.
//!
//! Gate mode (CI):
//!   unsafe_ratchet --geiger geiger.json --metadata metadata.json \
//!       --baseline .agents/baseline/unsafe-default.json
//! exits 2 and names the offending crates when exposure grew.
//!
//! Review mode (the auditable path a human takes to accept new exposure):
//!   ... --write-baseline .agents/baseline/unsafe-default.json
//! regenerates the pinned baseline; committing that diff is the review.
//! `--features <label>` names the pinned feature set (default: "default").

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use susi_gawd::unsafe_ratchet::{
    ratchet_unsafe, sbom_changes, GeigerVerdict, SbomEntry, UnsafeBaseline, UnsafeExposure,
};

struct Args {
    geiger: String,
    metadata: String,
    baseline: String,
    write_baseline: Option<String>,
    features: String,
}

fn parse_args() -> Result<Args, String> {
    let mut geiger = None;
    let mut metadata = None;
    let mut baseline = None;
    let mut write_baseline = None;
    let mut features = "default".to_string();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let value = match it.next() {
            Some(v) => v,
            None => return Err(format!("{arg} needs a value")),
        };
        match arg.as_str() {
            "--geiger" => geiger = Some(value),
            "--metadata" => metadata = Some(value),
            "--baseline" => baseline = Some(value),
            "--write-baseline" => write_baseline = Some(value),
            "--features" => features = value,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        geiger: geiger.ok_or("--geiger is required")?,
        metadata: metadata.ok_or("--metadata is required")?,
        baseline: baseline.ok_or("--baseline is required")?,
        write_baseline,
        features,
    })
}

fn read_json(path: &str) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))
}

/// Unsafe function count for one geiger package entry, tolerating the
/// schema drift between cargo-geiger releases: `used.functions.unsafe` is
/// the canonical path; fall back to any `unsafe` counter under `unsafety`.
fn unsafe_fns_of(unsafety: &serde_json::Value) -> u64 {
    fn count(v: &serde_json::Value) -> Option<u64> {
        v.get("used")
            .and_then(|u| u.get("functions"))
            .and_then(|f| f.get("unsafe"))
            .and_then(|n| n.as_u64())
            .or_else(|| {
                v.get("functions")
                    .and_then(|f| f.get("unsafe"))
                    .and_then(|n| n.as_u64())
            })
    }
    if let Some(n) = count(unsafety) {
        return n;
    }
    // Last resort: sum every {..,"unsafe":n} leaf under `unsafety.used`.
    let mut total = 0u64;
    if let Some(used) = unsafety.get("used").and_then(|u| u.as_object()) {
        for section in used.values() {
            if let Some(n) = section.get("unsafe").and_then(|n| n.as_u64()) {
                total = total.saturating_add(n);
            }
        }
    }
    total
}

/// Current exposure: one [`UnsafeExposure`] per package geiger scanned.
/// Workspace members (path sources) are first-party — their unsafe is
/// recorded for transparency but exempted from the gate, which exists to
/// catch third-party exposure growth; first-party unsafe is separately
/// governed by the justified-`SAFETY:` audit.
fn current_exposure(
    geiger: &serde_json::Value,
    first_party: &std::collections::BTreeSet<String>,
) -> Vec<UnsafeExposure> {
    let mut out = Vec::new();
    let packages = geiger
        .get("packages")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    for item in &packages {
        let name = item
            .get("package")
            .and_then(|p| p.get("name"))
            .or_else(|| item.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let unsafety = item.get("unsafety").cloned().unwrap_or_default();
        let unsafe_fns = unsafe_fns_of(&unsafety);
        if unsafe_fns == 0 {
            continue;
        }
        out.push(UnsafeExposure {
            crate_name: name.clone(),
            unsafe_fns: u32::try_from(unsafe_fns).unwrap_or(u32::MAX),
            first_party_exception: first_party.contains(&name),
        });
    }
    out
}

/// SBOM + first-party set from `cargo metadata` output.
fn sbom_from_metadata(
    metadata: &serde_json::Value,
) -> (Vec<SbomEntry>, std::collections::BTreeSet<String>) {
    let mut sbom = Vec::new();
    let mut first_party = std::collections::BTreeSet::new();
    if let Some(packages) = metadata.get("packages").and_then(|p| p.as_array()) {
        for pkg in packages {
            let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let provenance = match pkg.get("source") {
                Some(serde_json::Value::Null) | None => {
                    first_party.insert(name.to_string());
                    "path".to_string()
                }
                Some(serde_json::Value::String(s)) => s.clone(),
                _ => "unknown".to_string(),
            };
            sbom.push(SbomEntry {
                crate_name: name.to_string(),
                version: version.to_string(),
                provenance,
            });
        }
    }
    (sbom, first_party)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn run() -> Result<i32, String> {
    let args = parse_args()?;
    let geiger = read_json(&args.geiger)?;
    let metadata = read_json(&args.metadata)?;
    let (sbom, first_party) = sbom_from_metadata(&metadata);
    let current = current_exposure(&geiger, &first_party);

    if let Some(out_path) = &args.write_baseline {
        let baseline = UnsafeBaseline {
            features: args.features.clone(),
            exposure: current,
            sbom,
            recorded_unix: now_unix(),
        };
        let json = serde_json::to_string_pretty(&baseline).map_err(|e| e.to_string())?;
        std::fs::write(out_path, format!("{json}\n"))
            .map_err(|e| format!("could not write {out_path}: {e}"))?;
        println!(
            "unsafe-exposure baseline written to {out_path}: {} exposed crate(s), {} SBOM entr(ies), features={}",
            baseline.exposure.len(),
            baseline.sbom.len(),
            baseline.features
        );
        println!("review the diff and commit it — the commit is the acceptance record");
        return Ok(0);
    }

    let baseline: UnsafeBaseline = match std::fs::read_to_string(&args.baseline) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a valid baseline: {e}", args.baseline))?,
        Err(_) => UnsafeBaseline {
            features: args.features.clone(),
            exposure: Vec::new(),
            sbom: Vec::new(),
            recorded_unix: 0,
        },
    };
    if baseline.features != args.features {
        eprintln!(
            "warning: baseline pins features={} but this run measured features={}",
            baseline.features, args.features
        );
    }

    let changes = sbom_changes(&baseline.sbom, &sbom);
    if !changes.is_empty() {
        println!(
            "transitive SBOM changes since baseline ({}):",
            changes.len()
        );
        for c in &changes {
            match (&c.from, &c.to) {
                (Some(f), Some(t)) => println!("  {}: {} -> {}", c.crate_name, f, t),
                (None, Some(t)) => println!("  {}: new at {}", c.crate_name, t),
                (Some(f), None) => println!("  {}: removed (was {})", c.crate_name, f),
                (None, None) => {}
            }
        }
    }

    match ratchet_unsafe(&baseline.exposure, &current, &baseline.sbom, &sbom) {
        GeigerVerdict::Clean => {
            println!(
                "unsafe-exposure ratchet clean: {} exposed crate(s) within baseline",
                current.len()
            );
            Ok(0)
        }
        GeigerVerdict::NeedsReview { new_crates } => {
            eprintln!(
                "new unsafe exposure requires review: {}",
                new_crates.join(", ")
            );
            eprintln!(
                "to accept after review, regenerate the pinned baseline: \
                 unsafe_ratchet --geiger <geiger.json> --metadata <metadata.json> \
                 --write-baseline {} --features {}",
                args.baseline, args.features
            );
            Ok(2)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(2)),
        Err(e) => {
            eprintln!("unsafe_ratchet: {e}");
            ExitCode::from(2)
        }
    }
}
