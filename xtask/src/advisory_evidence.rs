//! Offline production advisories against an explicitly provisioned, pinned database.
//! The agent provisions an approved RustSec checkout; qualification never fetches it.
use serde_json::{json, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const DATABASE_DIRECTORY: &str = "advisory-db-3157b0e258782691";
const DATABASE_URL: &str = "https://github.com/RustSec/advisory-db";
const MAX_AGE_SECONDS: u64 = 7 * 24 * 60 * 60;
const CLOCK_SKEW_SECONDS: u64 = 300;

pub struct Source {
    path: PathBuf,
    revision: String,
    approved_origin: String,
}

impl Source {
    pub fn provisioned() -> Result<Self> {
        let path = env::var_os("SUSI_RELEASE_ADVISORY_DB")
            .map(PathBuf::from)
            .ok_or("provision a clean approved RustSec checkout in SUSI_RELEASE_ADVISORY_DB")?;
        let revision = env::var("SUSI_RELEASE_ADVISORY_REVISION")
            .map_err(|_| "pin the approved database commit in SUSI_RELEASE_ADVISORY_REVISION")?;
        let approved_origin = match env::var("SUSI_RELEASE_ADVISORY_ORIGIN") {
            Ok(value) => value,
            Err(env::VarError::NotPresent) => DATABASE_URL.to_owned(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            revision,
            approved_origin,
        })
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn validate_source(source: &Source, current_time: u64) -> Result<(String, u64)> {
    // This is an independently approved source identity, not an auto-discovered
    // origin. It is never contacted by qualification and grants no remote trust.
    if source.approved_origin.trim() != source.approved_origin
        || source.approved_origin.is_empty()
        || source.approved_origin.chars().any(char::is_control)
        || source.approved_origin.contains(['?', '#'])
        || source
            .approved_origin
            .split_once("://")
            .is_some_and(|(scheme, rest)| {
                let authority = rest.split('/').next().unwrap_or("");
                authority
                    .split_once('@')
                    .is_some_and(|(user, _)| scheme != "ssh" || user.contains(':'))
            })
    {
        return Err(
            "advisory database approved origin must be a nonempty credential-free identity".into(),
        );
    }
    if !super::hex(&source.revision, &[40, 64]) {
        return Err("advisory database revision must be a full Git object ID".into());
    }
    if !fs::metadata(&source.path)?.is_dir()
        || super::git(&source.path, &["rev-parse", "HEAD"])? != source.revision
    {
        return Err("advisory database does not match its approved revision".into());
    }
    let origin = super::git(&source.path, &["remote", "get-url", "origin"])?;
    if origin.strip_suffix(".git").unwrap_or(&origin)
        != source
            .approved_origin
            .strip_suffix(".git")
            .unwrap_or(&source.approved_origin)
    {
        return Err("advisory database does not identify its independently approved origin".into());
    }
    // No ignored overrides or untracked advisory records may enter the scan.
    if !super::git(
        &source.path,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?
    .is_empty()
    {
        return Err("advisory database must be clean, including ignored files".into());
    }
    let timestamp: u64 =
        super::git(&source.path, &["show", "-s", "--format=%ct", "HEAD"])?.parse()?;
    if timestamp > current_time.saturating_add(CLOCK_SKEW_SECONDS)
        || current_time.saturating_sub(timestamp) > MAX_AGE_SECONDS
    {
        return Err(
            "advisory database commit is stale (maximum seven days) or future-dated".into(),
        );
    }
    Ok((
        super::git(&source.path, &["rev-parse", "HEAD^{tree}"])?,
        timestamp,
    ))
}

fn content_receipt(path: &Path) -> Result<Value> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["ls-tree", "-rz", "--full-tree", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err("cannot enumerate pinned advisory database files".into());
    }
    let mut files = Vec::new();
    for entry in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty())
    {
        let entry = std::str::from_utf8(entry)?;
        let (metadata, name) = entry
            .split_once('\t')
            .ok_or("invalid database tree entry")?;
        if !(metadata.starts_with("100644 blob ") || metadata.starts_with("100755 blob ")) {
            return Err("advisory database may not contain symlinks or submodules".into());
        }
        let file = path.join(name);
        if !fs::symlink_metadata(&file)?.file_type().is_file() {
            return Err("advisory database file is missing or redirected".into());
        }
        files.push(json!({"path": name, "sha256": super::file_digest(&file)?}));
    }
    if files.is_empty() || !path.join("crates").is_dir() {
        return Err("advisory database has no tracked advisory collection".into());
    }
    Ok(
        json!({"file_count": files.len(), "files_sha256": super::digest(serde_json::to_string(&files)?.as_bytes())}),
    )
}

fn config_with_database(original: &str, database: &Path) -> Result<String> {
    // Preserve policy byte-for-byte. Reject existing database overrides rather
    // than parsing/reconstructing arbitrary policy or accidentally replacing it.
    if original.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with('#')
            && ["db-path", "db-urls", "db_path", "db_urls"]
                .iter()
                .any(|key| line.starts_with(key))
    }) {
        return Err(
            "qualification requires the standard RustSec database without policy DB overrides"
                .into(),
        );
    }
    let path = super::text(database)?;
    let setting = format!("db-path = {}\n", serde_json::to_string(path)?);
    let sections = original
        .lines()
        .filter(|line| line.trim() == "[advisories]")
        .count();
    match sections {
        0 => Ok(format!("{original}\n[advisories]\n{setting}")),
        1 => {
            let mut result = String::new();
            for line in original.split_inclusive('\n') {
                result.push_str(line);
                if line.trim() == "[advisories]" {
                    if !line.ends_with('\n') {
                        result.push('\n');
                    }
                    result.push_str(&setting);
                }
            }
            Ok(result)
        }
        _ => Err("ambiguous advisory policy section".into()),
    }
}

fn deny_executable() -> Result<PathBuf> {
    let path = env::var_os("PATH").ok_or("missing tool search path")?;
    let name = if cfg!(windows) {
        "cargo-deny.exe"
    } else {
        "cargo-deny"
    };
    for directory in env::split_paths(&path) {
        let executable = directory.join(name);
        if executable.is_file() {
            return Ok(executable.canonicalize()?);
        }
    }
    Err("cargo-deny must be provisioned before production qualification".into())
}

pub struct Snapshot {
    source: Source,
    config: PathBuf,
    config_sha256: String,
    pub executable: PathBuf,
    executable_sha256: String,
    content: Value,
    pub evidence: Value,
}

impl Snapshot {
    pub fn prepare(candidate: &Path, attempt: &Path, source: &Source) -> Result<Self> {
        let current_time = now()?;
        let (tree, committed_at) = validate_source(source, current_time)?;
        let approved_content = content_receipt(&source.path)?;
        let databases = attempt.join("advisory-dbs");
        fs::create_dir(&databases)?;
        let database = databases.join(DATABASE_DIRECTORY);
        // Local object copy, never network access, hardlinks or runtime state.
        // The standard RustSec URL's cargo-deny cache name is intentionally fixed;
        // an incompatible cargo-deny version fails instead of fetching another DB.
        let output = Command::new("git")
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.longpaths=true",
                "clone",
                // Private snapshot paths can exceed Windows legacy path limits.
                // Keep checkout/recheck support local to this owned clone.
                "--config",
                "core.longpaths=true",
                "--no-hardlinks",
                "--no-checkout",
                "--",
            ])
            .arg(&source.path)
            .arg(&database)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4096)]);
            return Err(format!(
                "cannot snapshot the approved advisory database ({}): {}",
                output.status,
                stderr.trim()
            )
            .into());
        }
        super::git(
            &database,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "core.autocrlf=false",
                "checkout",
                "--detach",
                &source.revision,
            ],
        )?;
        // The snapshot is an offline local copy; retain the checked source
        // identity instead of advertising its private filesystem path as origin.
        super::git(
            &database,
            &["remote", "set-url", "origin", &source.approved_origin],
        )?;
        let snapshot_source = Source {
            path: database,
            revision: source.revision.clone(),
            approved_origin: source.approved_origin.clone(),
        };
        validate_source(&snapshot_source, current_time)?;
        if content_receipt(&snapshot_source.path)? != approved_content
            || validate_source(source, current_time)? != (tree.clone(), committed_at)
        {
            return Err("approved advisory database changed during snapshot".into());
        }
        let original = fs::read_to_string(candidate.join("deny.toml"))?;
        let config_text = config_with_database(&original, &databases)?;
        let config = attempt.join("advisory-deny.toml");
        fs::write(&config, &config_text)?;
        let executable = deny_executable()?;
        let executable_sha256 = super::file_digest(&executable)?;
        let version = super::isolated_command(&executable, &attempt.join("advisory-tool-home"))?
            .arg("--version")
            .output()?;
        if !version.status.success() {
            return Err("cannot identify cargo-deny".into());
        }
        let version = String::from_utf8(version.stdout)?.trim().to_owned();
        if !version.starts_with("cargo-deny ") {
            return Err("unexpected cargo-deny version response".into());
        }
        let evidence = json!({
            "schema": "susi/production-advisories/v1",
            "database_url": DATABASE_URL,
            "approved_source_origin": source.approved_origin,
            "provenance": "caller-provisioned approved Git revision; configured origin checked; no network authentication is performed by this offline gate",
            "database_commit": source.revision, "database_tree": tree,
            "database_commit_unix": committed_at, "checked_unix": current_time,
            "maximum_age_seconds": MAX_AGE_SECONDS, "database_content": approved_content,
            "policy_sha256": super::digest(original.as_bytes()),
            "effective_config_sha256": super::digest(config_text.as_bytes()),
            "cargo_deny_version": version, "cargo_deny_sha256": executable_sha256,
            "scope": "root manifest normal/build dependency closure; native host; no default features; dev dependencies excluded"
        });
        fs::write(
            attempt.join("advisory-receipt.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        Ok(Self {
            source: snapshot_source,
            config,
            config_sha256: super::digest(config_text.as_bytes()),
            executable,
            executable_sha256,
            content: approved_content,
            evidence,
        })
    }

    pub fn recheck(&self) -> Result<()> {
        validate_source(&self.source, now()?)?;
        if content_receipt(&self.source.path)? != self.content
            || super::file_digest(&self.config)? != self.config_sha256
            || super::file_digest(&self.executable)? != self.executable_sha256
        {
            return Err("advisory database, policy or scanner changed during qualification".into());
        }
        Ok(())
    }
}

pub fn arguments(candidate: &Path, attempt: &Path, host: &str) -> Result<Vec<String>> {
    Ok([
        "deny",
        "--config",
        super::text(&attempt.join("advisory-deny.toml"))?,
        "--manifest-path",
        super::text(&candidate.join("Cargo.toml"))?,
        "--offline",
        "--locked",
        "--target",
        host,
        "--no-default-features",
        "--exclude-dev",
        "check",
        "advisories",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<String>>())
    .map(|mut args| {
        // Scan the graph that ships: the product's features, not the slim default.
        super::product_features::select_product_features(&mut args, candidate);
        args
    })
}

#[cfg(test)]
pub fn fixture_source(directory: &Path) -> Source {
    fixture_source_for(directory, "qualification-fixture")
}

#[cfg(test)]
pub fn fixture_source_for(directory: &Path, package: &str) -> Source {
    fs::create_dir(directory).unwrap();
    super::git(directory, &["init", "-q"]).unwrap();
    super::git(directory, &["config", "core.autocrlf", "false"]).unwrap();
    super::git(directory, &["remote", "add", "origin", DATABASE_URL]).unwrap();
    let collection = directory.join("crates").join(package);
    fs::create_dir_all(&collection).unwrap();
    fs::write(collection.join("RUSTSEC-2026-0001.md"),
        format!("```toml\n[advisory]\nid = \"RUSTSEC-2026-0001\"\npackage = \"{package}\"\ndate = \"2026-01-01\"\n[versions]\npatched = []\n```\n# Test-only vulnerable package\n\nFixture advisory, never used to qualify product source.\n")).unwrap();
    super::git(directory, &["add", "."]).unwrap();
    super::git(
        directory,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "core.hooksPath=",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture database",
        ],
    )
    .unwrap();
    Source {
        path: directory.to_owned(),
        revision: super::git(directory, &["rev-parse", "HEAD"]).unwrap(),
        approved_origin: DATABASE_URL.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SCRATCH_CREATE_ATTEMPTS: usize = 32;
    static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn create_scratch_at(
        base: &Path,
        stamp: &str,
        sequence: &AtomicU64,
    ) -> std::io::Result<PathBuf> {
        for _ in 0..SCRATCH_CREATE_ATTEMPTS {
            let id = sequence.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("susi-advisory-test-{stamp}-{id}"));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not reserve a unique advisory test directory after bounded retries",
        ))
    }

    fn scratch() -> PathBuf {
        create_scratch_at(
            &env::temp_dir(),
            &super::super::stamp().unwrap(),
            &SCRATCH_SEQUENCE,
        )
        .expect("reserve a unique advisory test directory")
    }

    #[test]
    fn scratch_skips_preexisting_directories_without_touching_them() {
        let base = env::temp_dir().join(format!(
            "susi-advisory-scratch-regression-{}-{}",
            std::process::id(),
            super::super::stamp().unwrap()
        ));
        fs::create_dir(&base).unwrap();
        let sequence = AtomicU64::new(0);
        let existing = base.join("susi-advisory-test-collision-0");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("preserve.txt"), b"keep").unwrap();

        let created = create_scratch_at(&base, "collision", &sequence).unwrap();
        assert_eq!(created, base.join("susi-advisory-test-collision-1"));
        assert_eq!(fs::read(existing.join("preserve.txt")).unwrap(), b"keep");
        assert!(created.is_dir());
    }

    #[test]
    fn scratch_allocations_are_unique_under_parallel_calls() {
        let base = env::temp_dir().join(format!(
            "susi-advisory-scratch-parallel-{}-{}",
            std::process::id(),
            super::super::stamp().unwrap()
        ));
        fs::create_dir(&base).unwrap();
        let sequence = AtomicU64::new(0);
        let paths = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32)
                .map(|_| scope.spawn(|| create_scratch_at(&base, "parallel", &sequence).unwrap()))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let unique: std::collections::BTreeSet<_> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len());
        assert!(paths.iter().all(|path| path.is_dir()));
    }

    #[test]
    fn database_pin_rejects_missing_wrong_dirty_stale_and_future_inputs() {
        let path = scratch();
        let mut source = fixture_source(&path.join("db"));
        let (_, committed_at) = validate_source(&source, now().unwrap()).unwrap();
        super::super::git(
            &source.path,
            &["remote", "set-url", "origin", "https://example.invalid/db"],
        )
        .unwrap();
        assert!(validate_source(&source, committed_at).is_err());
        super::super::git(&source.path, &["remote", "set-url", "origin", DATABASE_URL]).unwrap();
        assert!(validate_source(&source, committed_at + MAX_AGE_SECONDS + 1).is_err());
        assert!(validate_source(&source, committed_at - CLOCK_SKEW_SECONDS - 1).is_err());
        let revision = source.revision.clone();
        source.revision = "0".repeat(40);
        assert!(validate_source(&source, now().unwrap()).is_err());
        source.revision = revision;
        fs::write(source.path.join("unreviewed.md"), "unreviewed advisory").unwrap();
        assert!(validate_source(&source, now().unwrap()).is_err());
        fs::remove_file(source.path.join("unreviewed.md")).unwrap();
        fs::write(
            source
                .path
                .join("crates/qualification-fixture/RUSTSEC-2026-0001.md"),
            "altered tracked advisory",
        )
        .unwrap();
        assert!(validate_source(&source, now().unwrap()).is_err());
        source.path = path.join("missing");
        assert!(validate_source(&source, now().unwrap()).is_err());
    }

    #[test]
    fn database_configuration_preserves_policy_and_rejects_overrides() {
        let original = "[advisories]\nignore = []\n[licenses]\nallow = [\"MIT\"]\n";
        let generated = config_with_database(original, Path::new("C:\\private\\db")).unwrap();
        assert_eq!(
            generated.replace("db-path = \"C:\\\\private\\\\db\"\n", ""),
            original
        );
        assert!(
            config_with_database("[advisories]\ndb-path = \"other\"\n", Path::new("db")).is_err()
        );
        assert!(config_with_database("[advisories]\ndb-urls = []\n", Path::new("db")).is_err());
        assert!(config_with_database("[advisories]\n[advisories]\n", Path::new("db")).is_err());
    }

    #[test]
    fn approved_mirror_identity_is_explicit_and_does_not_relax_pin_or_freshness() {
        let path = scratch();
        let mut source = fixture_source(&path.join("db"));
        let mirror = "https://forge.example.invalid/security/rustsec.git";
        super::super::git(&source.path, &["remote", "set-url", "origin", mirror]).unwrap();
        assert!(validate_source(&source, now().unwrap()).is_err());
        source.approved_origin = mirror.to_owned();
        let (_, committed_at) = validate_source(&source, now().unwrap()).unwrap();
        assert!(validate_source(&source, committed_at + MAX_AGE_SECONDS + 1).is_err());
        source.revision = "0".repeat(40);
        assert!(validate_source(&source, now().unwrap()).is_err());
        for invalid in [
            "",
            " ",
            "https://forge.example.invalid/db\n",
            "https://user:secret@forge.example.invalid/db",
            "https://forge.example.invalid/db?token=secret",
            "ssh://user:secret@forge.example.invalid/db",
        ] {
            source.approved_origin = invalid.to_owned();
            assert!(validate_source(&source, now().unwrap()).is_err());
        }
    }

    #[test]
    fn mirror_snapshot_retains_source_identity_and_offline_scanner_lookup() {
        let path = scratch();
        // Fabricated fixture vulnerability, not an advisory about real autocfg.
        let mut source = fixture_source_for(&path.join("db"), "autocfg");
        source.approved_origin = "https://forge.example.invalid/security/rustsec".into();
        super::super::git(
            &source.path,
            &["remote", "set-url", "origin", &source.approved_origin],
        )
        .unwrap();
        let candidate = path.join("candidate");
        let attempt = path.join("attempt");
        fs::create_dir(&candidate).unwrap();
        fs::create_dir(&attempt).unwrap();
        fs::write(candidate.join("deny.toml"), "[advisories]\nignore=[]\n").unwrap();
        fs::write(candidate.join("Cargo.toml"), "[package]\nname=\"mirror-scan-fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nautocfg=\"=1.5.1\"\n").unwrap();
        fs::create_dir(candidate.join("src")).unwrap();
        fs::write(candidate.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        let lock = super::super::isolated_command(Path::new("cargo"), &path.join("lock-home"))
            .unwrap()
            .current_dir(&candidate)
            .args(["generate-lockfile", "--offline"])
            .output()
            .unwrap();
        assert!(
            lock.status.success(),
            "{}",
            String::from_utf8_lossy(&lock.stderr)
        );
        let snapshot = Snapshot::prepare(&candidate, &attempt, &source).unwrap();
        assert_eq!(
            snapshot.evidence["approved_source_origin"],
            source.approved_origin
        );
        assert_eq!(snapshot.evidence["database_url"], DATABASE_URL);
        assert_eq!(
            super::super::git(&snapshot.source.path, &["remote", "get-url", "origin"]).unwrap(),
            source.approved_origin
        );
        snapshot.recheck().unwrap();
        let (host, _) = super::super::compiler(&candidate).unwrap();
        let scan = super::super::isolated_command(&snapshot.executable, &path.join("scan-home"))
            .unwrap()
            .current_dir(&candidate)
            .args(arguments(&candidate, &attempt, &host).unwrap())
            .output()
            .unwrap();
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&scan.stdout),
            String::from_utf8_lossy(&scan.stderr)
        );
        assert!(
            !scan.status.success(),
            "fixture advisory must reject qualification"
        );
        assert!(log.contains("RUSTSEC-2026-0001"), "{log}");
        snapshot.recheck().unwrap();
        super::super::git(
            &snapshot.source.path,
            &["remote", "set-url", "origin", DATABASE_URL],
        )
        .unwrap();
        assert!(snapshot.recheck().is_err());
    }

    #[test]
    fn snapshot_preserves_source_and_rechecks_beyond_windows_legacy_path_limit() {
        let path = scratch();
        let source = fixture_source(&path.join("db"));
        let source_config = fs::read(source.path.join(".git/config")).unwrap();
        let candidate = path.join("candidate");
        fs::create_dir(&candidate).unwrap();
        fs::write(candidate.join("deny.toml"), "[advisories]\nignore=[]\n").unwrap();
        let mut attempt = path.join("attempt");
        while attempt
            .join("advisory-dbs")
            .join(DATABASE_DIRECTORY)
            .to_string_lossy()
            .encode_utf16()
            .count()
            < 220
        {
            attempt = attempt.join("nested-snapshot");
        }
        fs::create_dir_all(&attempt).unwrap();
        let snapshot = Snapshot::prepare(&candidate, &attempt, &source).unwrap();
        let advisory = Path::new("crates/qualification-fixture/RUSTSEC-2026-0001.md");
        let copied_advisory = snapshot.source.path.join(advisory);
        assert!(copied_advisory.to_string_lossy().encode_utf16().count() > 260);
        assert_eq!(
            fs::read(copied_advisory).unwrap(),
            fs::read(source.path.join(advisory)).unwrap()
        );
        snapshot.recheck().unwrap();
        assert_eq!(
            fs::read(source.path.join(".git/config")).unwrap(),
            source_config
        );
        validate_source(&source, now().unwrap()).unwrap();
    }

    #[test]
    fn snapshot_detects_modified_database_and_policy() {
        let path = scratch();
        let source = fixture_source(&path.join("db"));
        let candidate = path.join("candidate");
        let attempt = path.join("attempt");
        fs::create_dir(&candidate).unwrap();
        fs::create_dir(&attempt).unwrap();
        fs::write(candidate.join("deny.toml"), "[advisories]\nignore=[]\n").unwrap();
        let mut snapshot = Snapshot::prepare(&candidate, &attempt, &source).unwrap();
        snapshot.recheck().unwrap();
        let original_executable = snapshot.executable.clone();
        let private_executable = path.join("private-scanner-copy");
        fs::copy(&original_executable, &private_executable).unwrap();
        snapshot.executable = private_executable.clone();
        snapshot.recheck().unwrap();
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(private_executable)
            .unwrap()
            .write_all(b"tampered scanner")
            .unwrap();
        assert!(snapshot.recheck().is_err());
        snapshot.executable = original_executable;
        snapshot.recheck().unwrap();
        let original = fs::read(&snapshot.config).unwrap();
        fs::write(
            &snapshot.config,
            "[advisories]\nignore=['RUSTSEC-2023-0071']\n",
        )
        .unwrap();
        assert!(snapshot.recheck().is_err());
        fs::write(&snapshot.config, original).unwrap();
        let advisory = snapshot
            .source
            .path
            .join("crates/qualification-fixture/RUSTSEC-2026-0001.md");
        fs::write(advisory, "removed advisory").unwrap();
        assert!(snapshot.recheck().is_err());
        assert!(validate_source(&source, now().unwrap()).is_ok());
    }
}
