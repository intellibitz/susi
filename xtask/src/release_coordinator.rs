//! Laptop-held release coordinator (CEO decision 2026-11-06, "agent-built,
//! laptop-held"). `local-release qualify` admits a candidate only through a
//! trust manifest that the candidate cannot author: gate tools built from a
//! previously released source, the provenance verifier that actually recorded
//! the candidate's integration receipts, and an immutable forward-adoption
//! permit. This module produces that manifest and registry outside the
//! repository and refuses to register anything the pinned verifier rejects.
//!
//! Independence boundaries, stated exactly:
//! - `unsafe_measure` / `unsafe_ratchet` are compiled from an earlier released
//!   commit (newest reachable `v*` tag by default), never from the candidate.
//! - The provenance verifier must be byte-identical to the `local-workflow`
//!   executable that recorded every admitted receipt (`coordinator_sha256`), so
//!   its independence comes from a held snapshot pinned by digest, not from a
//!   rebuild.
//! - Commits at or before the forward-adoption baseline are adopted with
//!   `historical_ownership: incomplete`; the diagnostics blob lists what lacks
//!   native receipts since the last promoted release.
use super::{
    compiler, digest, file_digest, git, git_input, hex, resolve, stamp, text, Context, Manifest,
    Result, CURRENT, INTEGRATED,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const TRUST_SCHEMA: &str = "susi/release-gate-trust/v1";
const PERMIT_SCHEMA: &str = "susi-native-provenance-permit-v1";
const RECEIPT_SCHEMA: &str = "susi-native-provenance-receipt-v1";
const DIAGNOSTICS_SCHEMA: &str = "susi/forward-adoption-diagnostics/v1";
const TOOLS_SCHEMA: &str = "susi/release-coordinator-tools/v1";
const RECORD_SCHEMA: &str = "susi/release-coordination/v1";
const RECEIPTS: &str = "refs/susi/provenance/receipts/";
const EVIDENCE: &str = "refs/susi/evidence/objects/";
const TOOLS_CRATE: &str = "susi-unsafe-ratchet";
const TOOLS: [&str; 2] = ["unsafe_measure", "unsafe_ratchet"];
const USAGE: &str = "usage: local-release coordinate [--head <rev>] [--tools-source <rev>] [--verifier <local-workflow executable>] [--home <directory outside the repository>]";

#[derive(Default)]
struct Options {
    head: Option<String>,
    tools_source: Option<String>,
    verifier: Option<PathBuf>,
    home: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(USAGE)?;
        let duplicate = match flag.as_str() {
            "--head" => options.head.replace(value.clone()).is_some(),
            "--tools-source" => options.tools_source.replace(value.clone()).is_some(),
            "--verifier" => options.verifier.replace(value.into()).is_some(),
            "--home" => options.home.replace(value.into()).is_some(),
            _ => return Err(USAGE.into()),
        };
        if duplicate {
            return Err(format!("duplicate {flag}; {USAGE}").into());
        }
    }
    Ok(options)
}

/// The per-user coordinator directory. Zero configuration: the platform's
/// local application-data directory, never the repository.
fn default_home() -> Result<PathBuf> {
    if cfg!(windows) {
        let base = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
        return Ok(PathBuf::from(base)
            .join("SusiLocal")
            .join("release-coordinator"));
    }
    if let Some(base) = env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(base)
            .join("SusiLocal")
            .join("release-coordinator"));
    }
    let home = env::var_os("HOME").ok_or("HOME is unavailable")?;
    Ok(PathBuf::from(home)
        .join(".local/share")
        .join("SusiLocal")
        .join("release-coordinator"))
}

/// Trusted inputs live outside every checkout that could be a candidate.
fn require_outside(path: &Path, roots: &[&Path]) -> Result<()> {
    let path = path.canonicalize()?;
    for root in roots {
        if path.starts_with(root.canonicalize()?) {
            return Err(format!(
                "coordinator state {} must live outside the repository {}",
                path.display(),
                root.display()
            )
            .into());
        }
    }
    Ok(())
}

fn primary_root(ctx: &Context) -> Result<PathBuf> {
    // ctx.state is <common git dir>/susi-local-release.
    Ok(ctx
        .state
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot locate the primary checkout")?
        .to_owned())
}

fn commit(root: &Path, revision: &str) -> Result<String> {
    git(
        root,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
}

fn tree(root: &Path, commit: &str) -> Result<String> {
    git(root, &["rev-parse", &format!("{commit}^{{tree}}")])
}

fn is_ancestor(root: &Path, before: &str, after: &str) -> Result<bool> {
    let status = Command::new("git")
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", before, after])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("cannot compare ancestry of {before} and {after}").into()),
    }
}

fn rev_list(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let mut all = vec!["rev-list"];
    all.extend_from_slice(args);
    Ok(git(root, &all)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Retain bytes as a Git blob with its own direct evidence ref, exactly as the
/// provenance verifier requires (`refs/susi/evidence/objects/<oid>`).
fn store_evidence_blob(root: &Path, bytes: &[u8]) -> Result<String> {
    let object = git_input(root, &["hash-object", "-w", "--stdin"], bytes)?;
    if !hex(&object, &[40, 64]) {
        return Err("git returned an invalid evidence object id".into());
    }
    let reference = format!("{EVIDENCE}{object}");
    let existing = git(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(symref)",
            &reference,
        ],
    )?;
    if existing.is_empty() {
        let zero = "0".repeat(object.len());
        git(
            root,
            &["update-ref", "--no-deref", &reference, &object, &zero],
        )?;
    } else if existing != object {
        return Err(format!("conflicting or symbolic evidence reference {reference}").into());
    }
    Ok(object)
}

fn evidence_rooted(root: &Path, object: &str) -> Result<bool> {
    Ok(git(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(symref)",
            &format!("{EVIDENCE}{object}"),
        ],
    )? == object)
}

/// Every native provenance receipt in the repository, as `(blob, receipt)`.
fn all_receipts(root: &Path) -> Result<Vec<(String, Value)>> {
    let listing = git(root, &["for-each-ref", "--format=%(objectname)", RECEIPTS])?;
    let mut receipts = Vec::new();
    for object in listing.lines().filter(|line| !line.is_empty()) {
        let value: Value = serde_json::from_str(&git(root, &["cat-file", "blob", object])?)
            .map_err(|error| format!("unreadable native receipt {object}: {error}"))?;
        receipts.push((object.to_owned(), value));
    }
    Ok(receipts)
}

fn field<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str> {
    let mut current = value;
    for key in path {
        current = current
            .get(key)
            .ok_or_else(|| format!("native receipt lacks {}", path.join(".")))?;
    }
    current
        .as_str()
        .ok_or_else(|| format!("native receipt field {} is not a string", path.join(".")).into())
}

/// The receipts that accept the candidate head, and the forward-adoption
/// baseline they imply.
#[derive(Debug)]
struct Adoption {
    receipts: Vec<String>,
    workers: Vec<String>,
    coordinator: String,
    policy: String,
    baseline: String,
}

fn adoption(root: &Path, head: &str, receipts: &[(String, Value)]) -> Result<Adoption> {
    let mut selected = Vec::new();
    for (object, receipt) in receipts {
        if receipt
            .pointer("/integration/commit")
            .and_then(Value::as_str)
            == Some(head)
        {
            selected.push((object, receipt));
        }
    }
    if selected.is_empty() {
        return Err(format!(
            "candidate {head} has no native provenance receipt; integrate it with `local-workflow ship` under SUSI_LOCAL_GATE=full before coordinating"
        )
        .into());
    }
    let mut coordinators = BTreeSet::new();
    let mut policies = BTreeSet::new();
    let mut bounds = Vec::new();
    let mut workers = Vec::new();
    for (object, receipt) in &selected {
        if field(receipt, &["schema"])? != RECEIPT_SCHEMA {
            return Err(format!("receipt {object} has an unsupported schema").into());
        }
        if !evidence_rooted(root, object)? {
            return Err(format!("receipt {object} is not rooted as durable evidence").into());
        }
        let worker = field(receipt, &["worker"])?;
        let gates = receipt
            .get("gates")
            .and_then(Value::as_array)
            .ok_or("native receipt lacks gates")?;
        let clippy = gates
            .iter()
            .find(|gate| gate["id"] == "clippy")
            .and_then(|gate| gate["argv"].as_array())
            .ok_or("native receipt lacks its clippy gate")?;
        if !clippy.iter().any(|arg| arg == "--workspace") {
            return Err(format!(
                "receipt {object} ({worker}) ran affected-package gates; production provenance requires the whole-workspace gate plan, so re-integrate with SUSI_LOCAL_GATE=full"
            )
            .into());
        }
        let coordinator = field(receipt, &["coordinator_sha256"])?;
        if !hex(coordinator, &[64]) {
            return Err(format!("receipt {object} has an invalid coordinator digest").into());
        }
        coordinators.insert(coordinator.to_owned());
        policies.insert(field(receipt, &["policy_sha256"])?.to_owned());
        bounds.push(field(receipt, &["claim", "base"])?.to_owned());
        if let Some(previous) = receipt
            .pointer("/integration/previous")
            .and_then(Value::as_str)
        {
            bounds.push(previous.to_owned());
        }
        workers.push(worker.to_owned());
    }
    if coordinators.len() != 1 || policies.len() != 1 {
        return Err(
            "receipts at the candidate head were recorded by different coordinators or policies; one pinned verifier cannot admit them"
                .into(),
        );
    }
    let mut args = vec!["merge-base", "--octopus"];
    args.extend(bounds.iter().map(String::as_str));
    let baseline = if bounds.len() == 1 {
        commit(root, &bounds[0])?
    } else {
        git(root, &args)?
    };
    if !is_ancestor(root, &baseline, head)? || baseline == head {
        return Err("forward-adoption baseline must be a strict ancestor of the candidate".into());
    }
    Ok(Adoption {
        receipts: selected.iter().map(|(o, _)| (*o).clone()).collect(),
        workers,
        coordinator: coordinators.into_iter().next().ok_or("no coordinator")?,
        policy: policies.into_iter().next().ok_or("no policy")?,
        baseline,
    })
}

/// The held `local-workflow` executable whose digest equals the receipts'
/// coordinator digest. A matching binary outside the held directory is
/// snapshotted into it first; nothing is ever rebuilt to match.
fn pinned_verifier(
    home: &Path,
    candidates: &[PathBuf],
    explicit: Option<&Path>,
    want: &str,
) -> Result<PathBuf> {
    let held = home.join("verifiers");
    fs::create_dir_all(&held)?;
    let mut search: Vec<PathBuf> = Vec::new();
    if let Some(path) = explicit {
        search.push(path.to_owned());
    } else {
        for entry in fs::read_dir(&held)? {
            let path = entry?.path();
            if path.is_file() {
                search.push(path);
            }
        }
        search.extend(candidates.iter().cloned());
    }
    for path in search {
        if !path.is_file() || file_digest(&path)? != want {
            continue;
        }
        if path.parent().and_then(|p| p.canonicalize().ok()) == Some(held.canonicalize()?) {
            return Ok(path);
        }
        let snapshot = held.join(format!(
            "local-workflow-{}{}",
            &want[..12],
            env::consts::EXE_SUFFIX
        ));
        if snapshot.exists() {
            if file_digest(&snapshot)? != want {
                return Err(format!("held verifier {} was altered", snapshot.display()).into());
            }
        } else {
            fs::copy(&path, &snapshot)?;
            read_only(&snapshot)?;
        }
        if file_digest(&snapshot)? != want {
            return Err("verifier snapshot does not match its source".into());
        }
        return Ok(snapshot);
    }
    Err(format!(
        "no held or local local-workflow executable has the receipts' coordinator digest {want}; the integrating binary was not snapshotted before it was rebuilt, so re-integrate with a held local-workflow executable"
    )
    .into())
}

fn read_only(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

/// The policy digest compiled into the pinned verifier itself.
fn verifier_policy(verifier: &Path, root: &Path) -> Result<String> {
    let output = Command::new(verifier)
        .current_dir(root)
        .arg("provenance-policy")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "pinned verifier cannot report its policy: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let report: Value = serde_json::from_slice(&output.stdout)?;
    let policy = report["policy"]
        .as_str()
        .ok_or("pinned verifier policy is missing")?;
    let pin = report["policy_sha256"]
        .as_str()
        .ok_or("pinned verifier policy digest is missing")?;
    if digest(policy.as_bytes()) != pin {
        return Err("pinned verifier reports a policy that does not match its digest".into());
    }
    Ok(pin.to_owned())
}

/// The previously released source the gate tools are built from: an explicit
/// revision, or the newest `v*` tag reachable from the candidate. It must be a
/// strict ancestor of the candidate.
fn tools_source(root: &Path, explicit: Option<&str>, head: &str) -> Result<(String, String)> {
    let (label, source) = match explicit {
        Some(revision) => (revision.to_owned(), commit(root, revision)?),
        None => {
            let tags = git(
                root,
                &[
                    "tag",
                    "--list",
                    "v*",
                    "--merged",
                    head,
                    "--sort=-creatordate",
                ],
            )?;
            let tag = tags
                .lines()
                .find(|tag| !tag.is_empty())
                .ok_or("no released v* tag precedes the candidate; pass --tools-source")?;
            (tag.to_owned(), commit(root, tag)?)
        }
    };
    if source == head || !is_ancestor(root, &source, head)? {
        return Err("gate tools must be built from a released ancestor, not the candidate".into());
    }
    git(
        root,
        &[
            "cat-file",
            "-e",
            &format!("{source}:crates/{TOOLS_CRATE}/Cargo.toml"),
        ],
    )
    .map_err(|_| format!("{label} does not contain {TOOLS_CRATE}"))?;
    Ok((label, source))
}

fn held_tools(record: &Path) -> Result<Option<Value>> {
    if !record.is_file() {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&fs::read(record)?)?;
    for name in TOOLS {
        let path = value
            .pointer(&format!("/tools/{name}/path"))
            .and_then(Value::as_str)
            .ok_or("held tool record is incomplete")?;
        let pin = value
            .pointer(&format!("/tools/{name}/sha256"))
            .and_then(Value::as_str)
            .ok_or("held tool record is incomplete")?;
        if file_digest(Path::new(path))? != pin {
            return Err(format!("held gate tool {path} changed after it was built").into());
        }
    }
    Ok(Some(value))
}

/// Build the unsafe gate tools from the released source in a private detached
/// checkout and private target directory, then hold them read-only.
fn gate_tools(root: &Path, home: &Path, label: &str, source: &str, host: &str) -> Result<Value> {
    let held = home.join("tools").join(source);
    let record = held.join("build.json");
    if let Some(value) = held_tools(&record)? {
        return Ok(value);
    }
    let build = home.join("build");
    fs::create_dir_all(&build)?;
    let checkout = build.join(format!("src-{}", &source[..12]));
    if checkout.exists() {
        let _ = git(root, &["worktree", "remove", "--force", text(&checkout)?]);
        if checkout.exists() {
            fs::remove_dir_all(&checkout)?;
        }
        // Only this checkout's record, never a repository-wide prune.
        super::worktree_admin::forget(root, &checkout)?;
    }
    git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "--detach",
            text(&checkout)?,
            source,
        ],
    )?;
    let target = build.join("target");
    let log_path = build.join(format!("build-{}.log", &source[..12]));
    let built = (|| -> Result<(String, String, u128)> {
        let version = |program: &str, flag: &str| -> Result<String> {
            let output = Command::new(program)
                .current_dir(&checkout)
                .arg(flag)
                .output()?;
            if !output.status.success() {
                return Err(format!("{program} {flag} failed").into());
            }
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        let rustc = version("rustc", "-vV")?;
        let cargo = version("cargo", "-V")?;
        let log = fs::File::create(&log_path)?;
        let started = Instant::now();
        let status = Command::new("cargo")
            .current_dir(&checkout)
            .args([
                "build",
                "-p",
                TOOLS_CRATE,
                "--bins",
                "--release",
                "--offline",
                "--locked",
                "--target",
                host,
                "--target-dir",
                text(&target)?,
            ])
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_TARGET_DIR", &target)
            .env("CARGO_BUILD_BUILD_DIR", &target)
            .env("RUSTC_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .status()?;
        if !status.success() {
            return Err(format!(
                "building gate tools from {label} failed; log {}",
                log_path.display()
            )
            .into());
        }
        Ok((rustc, cargo, started.elapsed().as_millis()))
    })();
    let _ = git(root, &["worktree", "remove", "--force", text(&checkout)?]);
    let (rustc, cargo, elapsed_ms) = built?;
    fs::create_dir_all(&held)?;
    let mut tools = serde_json::Map::new();
    for name in TOOLS {
        let file = format!("{name}{}", env::consts::EXE_SUFFIX);
        let destination = held.join(&file);
        if destination.exists() {
            return Err(format!(
                "{} exists without a complete build record; inspect it before rebuilding",
                destination.display()
            )
            .into());
        }
        fs::copy(target.join(host).join("release").join(&file), &destination)?;
        read_only(&destination)?;
        tools.insert(
            name.to_owned(),
            json!({"path": text(&destination)?, "sha256": file_digest(&destination)?}),
        );
    }
    let value = json!({
        "schema": TOOLS_SCHEMA,
        "crate": TOOLS_CRATE,
        "source": {"label": label, "commit": source, "tree": tree(root, source)?},
        "target": host,
        "profile": "release",
        "rustc": rustc,
        "cargo": cargo,
        "build_elapsed_ms": elapsed_ms,
        "build_log_sha256": file_digest(&log_path)?,
        "tools": tools,
    });
    fs::write(&record, serde_json::to_vec_pretty(&value)?)?;
    read_only(&record)?;
    Ok(value)
}

/// Honest record of what the forward adoption leaves without native
/// ownership: commits since the last promoted release, up to the baseline,
/// that no native provenance receipt covers.
fn diagnostics(
    root: &Path,
    head: &str,
    adoption: &Adoption,
    receipts: &[(String, Value)],
    release_source: Option<&str>,
) -> Result<Vec<u8>> {
    let mut covered = BTreeSet::new();
    for (_, receipt) in receipts {
        let (Ok(scope_base), Ok(source)) = (
            field(receipt, &["claim", "scope_base"]),
            field(receipt, &["source", "commit"]),
        ) else {
            continue;
        };
        if let Ok(commits) = rev_list(root, &[&format!("{scope_base}..{source}")]) {
            covered.extend(commits);
        }
    }
    let window = match release_source {
        Some(source) if is_ancestor(root, source, &adoption.baseline)? => Some(rev_list(
            root,
            &["--no-merges", &format!("{source}..{}", adoption.baseline)],
        )?),
        Some(_) | None => None,
    };
    let unreceipted: Vec<&String> = window
        .iter()
        .flatten()
        .filter(|commit| !covered.contains(*commit))
        .collect();
    let value = json!({
        "schema": DIAGNOSTICS_SCHEMA,
        "candidate": {"commit": head, "tree": tree(root, head)?},
        "baseline": {"commit": adoption.baseline, "tree": tree(root, &adoption.baseline)?},
        "historical_ownership": "incomplete",
        "reason": "Native provenance receipts exist only for integrations recorded by the native local-workflow coordinator. History at or before the baseline is adopted forward without complete per-commit ownership evidence; commits after it are covered by the admitted receipts.",
        "history_commits_at_baseline": rev_list(root, &["--count", &adoption.baseline])?.first().cloned().unwrap_or_default(),
        "native_receipts_in_repository": receipts.len(),
        "admitted_receipts": adoption.receipts,
        "admitted_workers": adoption.workers,
        "last_promoted_release_source": release_source,
        "unreceipted_since_last_promoted_release": {
            "measured": window.is_some(),
            "count": unreceipted.len(),
            "commits": unreceipted,
        },
    });
    Ok(serde_json::to_vec_pretty(&value)?)
}

/// Content-addressed, never-overwritten registry object.
fn register(registry: &Path, kind: &str, bytes: &[u8]) -> Result<PathBuf> {
    let directory = registry.join(kind);
    fs::create_dir_all(&directory)?;
    let pin = digest(bytes);
    let path = directory.join(format!("{pin}.json"));
    if fs::symlink_metadata(&path).is_ok() {
        if !fs::symlink_metadata(&path)?.file_type().is_file() || fs::read(&path)? != bytes {
            return Err(format!("registry object {} was altered", path.display()).into());
        }
        return Ok(path);
    }
    let staging = directory.join(format!(".{pin}.{}.tmp", stamp()?));
    fs::write(&staging, bytes)?;
    fs::rename(&staging, &path)?;
    read_only(&path)?;
    Ok(path)
}

/// Run the pinned verifier on the staged permit. Only a `verified: true`
/// report lets the permit and manifest enter the registry.
fn preflight(
    verifier: &Path,
    root: &Path,
    permit: &Path,
    pins: (&str, &str, &str),
    head: &str,
) -> Result<Value> {
    let (permit_sha256, verifier_sha256, policy_sha256) = pins;
    let output = Command::new(verifier)
        .current_dir(root)
        .args([
            "verify-provenance",
            "--root",
            text(root)?,
            "--permit",
            text(permit)?,
            "--permit-sha256",
            permit_sha256,
            "--verifier-sha256",
            verifier_sha256,
            "--policy-sha256",
            policy_sha256,
            "--head",
            head,
        ])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "pinned verifier rejected the candidate's provenance: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let report: Value = serde_json::from_slice(&output.stdout)?;
    if report["verified"] != true
        || report["permit_sha256"] != permit_sha256
        || report["source"]["commit"] != head
    {
        return Err("pinned verifier did not verify this permit and candidate".into());
    }
    Ok(report)
}

fn last_release_source(ctx: &Context) -> Result<Option<String>> {
    let Some(blob) = resolve(&ctx.root, CURRENT)? else {
        return Ok(None);
    };
    let manifest = Manifest::decode(&(git(&ctx.root, &["cat-file", "blob", &blob])? + "\n"))?;
    Ok(Some(manifest.source))
}

pub(super) fn run(ctx: &Context, args: &[String]) -> Result<()> {
    let options = parse(args)?;
    let started = Instant::now();
    let home = match options.home {
        Some(home) => home,
        None => default_home()?,
    };
    fs::create_dir_all(&home)?;
    let primary = primary_root(ctx)?;
    require_outside(&home, &[&ctx.root, &primary])?;
    let head = commit(&ctx.root, options.head.as_deref().unwrap_or(INTEGRATED))?;
    let head_tree = tree(&ctx.root, &head)?;
    let receipts = all_receipts(&ctx.root)?;
    let adoption = adoption(&ctx.root, &head, &receipts)?;
    let local = primary
        .join("target")
        .join("debug")
        .join(format!("local-workflow{}", env::consts::EXE_SUFFIX));
    let verifier = pinned_verifier(
        &home,
        &[local],
        options.verifier.as_deref(),
        &adoption.coordinator,
    )?;
    let policy = verifier_policy(&verifier, &ctx.root)?;
    if policy != adoption.policy {
        return Err("pinned verifier policy differs from the receipts' policy".into());
    }
    let (host, _) = compiler(&ctx.root)?;
    let (label, source) = tools_source(&ctx.root, options.tools_source.as_deref(), &head)?;
    let tools = gate_tools(&primary, &home, &label, &source, &host)?;
    let release_source = last_release_source(ctx)?;
    let diagnostics_bytes = diagnostics(
        &ctx.root,
        &head,
        &adoption,
        &receipts,
        release_source.as_deref(),
    )?;
    let diagnostics_blob = store_evidence_blob(&ctx.root, &diagnostics_bytes)?;
    let permit = json!({
        "schema": PERMIT_SCHEMA,
        "source": {"commit": head, "tree": head_tree},
        "verifier_sha256": adoption.coordinator,
        "policy_sha256": policy,
        "baseline": {
            "commit": adoption.baseline,
            "tree": tree(&ctx.root, &adoption.baseline)?,
            "historical_ownership": "incomplete",
            "diagnostics_blob": diagnostics_blob,
            "diagnostics_sha256": digest(&diagnostics_bytes),
        },
        "receipts": adoption.receipts,
    });
    let permit_bytes = serde_json::to_vec_pretty(&permit)?;
    let permit_sha256 = digest(&permit_bytes);
    let staging = home.join("staging");
    fs::create_dir_all(&staging)?;
    let staged = staging.join(format!("permit-{}.json", stamp()?));
    fs::write(&staged, &permit_bytes)?;
    let report = preflight(
        &verifier,
        &ctx.root,
        &staged,
        (&permit_sha256, &adoption.coordinator, &policy),
        &head,
    );
    fs::remove_file(&staged)?;
    let report = report?;
    let registry = home.join("registry");
    let permit_path = register(&registry, "permits", &permit_bytes)?;
    let tool = |name: &str| -> Result<Value> {
        Ok(json!({
            "path": tools.pointer(&format!("/tools/{name}/path")).ok_or("missing held tool")?,
            "sha256": tools.pointer(&format!("/tools/{name}/sha256")).ok_or("missing held tool")?,
            "built_from": tools["source"],
        }))
    };
    let manifest = json!({
        "schema": TRUST_SCHEMA,
        "source": {"commit": head, "tree": head_tree},
        "unsafe_measure": tool("unsafe_measure")?,
        "unsafe_ratchet": tool("unsafe_ratchet")?,
        "provenance": {
            "path": text(&verifier)?,
            "sha256": adoption.coordinator,
            "policy_sha256": policy,
            "permit_path": text(&permit_path)?,
            "permit_sha256": permit_sha256,
        },
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let manifest_sha256 = digest(&manifest_bytes);
    let manifest_path = register(&registry, "gate-trust", &manifest_bytes)?;
    let records = home.join("coordinations");
    fs::create_dir_all(&records)?;
    let record = records.join(format!("{head}-{}.json", stamp()?));
    fs::write(
        &record,
        serde_json::to_vec_pretty(&json!({
            "schema": RECORD_SCHEMA,
            "candidate": {"commit": head, "tree": head_tree},
            "registry": text(&registry)?,
            "manifest": {"path": text(&manifest_path)?, "sha256": manifest_sha256},
            "permit": {"path": text(&permit_path)?, "sha256": permit_sha256},
            "diagnostics": {"blob": diagnostics_blob, "sha256": digest(&diagnostics_bytes)},
            "tools": tools,
            "verifier": {"path": text(&verifier)?, "sha256": adoption.coordinator},
            "preflight": report,
            "elapsed_ms": started.elapsed().as_millis(),
        }))?,
    )?;
    eprintln!(
        "Coordinated {head}: verifier verified the permit; gate tools from {label} ({source}); record {}.",
        record.display()
    );
    println!("SUSI_RELEASE_TRUST_REGISTRY={}", registry.display());
    println!("SUSI_RELEASE_GATE_TRUST={}", manifest_path.display());
    println!("SUSI_RELEASE_GATE_TRUST_SHA256={manifest_sha256}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("susi-coordinator-{name}-{}", stamp().unwrap()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn repo(name: &str) -> PathBuf {
        let root = scratch(name);
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "coordinator@test"],
            vec!["config", "user.name", "coordinator"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["config", "tag.gpgsign", "false"],
        ] {
            git(&root, &args).unwrap();
        }
        root
    }

    fn commit_file(root: &Path, path: &str, body: &str) -> String {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
        git(root, &["add", "-A"]).unwrap();
        git(root, &["commit", "-q", "-m", path]).unwrap();
        git(root, &["rev-parse", "HEAD"]).unwrap()
    }

    #[allow(clippy::too_many_arguments)] // fixture builder: each argument is one receipt field a test varies
    fn receipt(
        root: &Path,
        worker: &str,
        source: &str,
        head: &str,
        previous: &str,
        coordinator: &str,
        whole: bool,
    ) -> String {
        let mut clippy = vec!["cargo", "clippy"];
        if whole {
            clippy.push("--workspace");
        } else {
            clippy.extend(["-p", "susi"]);
        }
        let value = json!({"schema":RECEIPT_SCHEMA,"worker":worker,"coordinator_sha256":coordinator,
            "policy_sha256":"a".repeat(64),
            "claim":{"base":previous,"scope_base":previous,"scopes":["src"],"blob":"0".repeat(40)},
            "source":{"commit":source},"integration":{"previous":previous,"commit":head},
            "gates":[{"id":"clippy","argv":clippy}]});
        let object = store_evidence_blob(root, &serde_json::to_vec(&value).unwrap()).unwrap();
        git(
            root,
            &[
                "update-ref",
                &format!("{RECEIPTS}{worker}/{source}"),
                &object,
            ],
        )
        .unwrap();
        object
    }

    #[test]
    fn coordinator_registry_objects_are_content_addressed_and_never_overwritten() {
        let registry = scratch("registry");
        let first = register(&registry, "permits", b"{\"a\":1}").unwrap();
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            format!("{}.json", digest(b"{\"a\":1}"))
        );
        assert_eq!(register(&registry, "permits", b"{\"a\":1}").unwrap(), first);
        let mut permissions = fs::metadata(&first).unwrap().permissions();
        assert!(permissions.readonly());
        #[allow(clippy::permissions_set_readonly_false)] // test tampers deliberately
        permissions.set_readonly(false);
        fs::set_permissions(&first, permissions).unwrap();
        fs::write(&first, b"{\"a\":2}").unwrap();
        let error = register(&registry, "permits", b"{\"a\":1}").unwrap_err();
        assert!(error.to_string().contains("altered"), "{error}");
    }

    #[test]
    fn coordinator_evidence_blobs_have_direct_durable_refs() {
        let root = repo("evidence");
        commit_file(&root, "a.txt", "a");
        let object = store_evidence_blob(&root, b"diagnostics").unwrap();
        assert!(evidence_rooted(&root, &object).unwrap());
        assert_eq!(store_evidence_blob(&root, b"diagnostics").unwrap(), object);
        assert_eq!(
            git(&root, &["cat-file", "blob", &object]).unwrap(),
            "diagnostics"
        );
    }

    #[test]
    fn coordinator_admits_only_whole_workspace_receipts_from_one_coordinator() {
        let root = repo("adoption");
        let base = commit_file(&root, "a.txt", "a");
        let source = commit_file(&root, "src/b.txt", "b");
        let one = "1".repeat(64);
        let error = adoption(&root, &source, &all_receipts(&root).unwrap()).unwrap_err();
        assert!(
            error.to_string().contains("no native provenance receipt"),
            "{error}"
        );

        let narrowed = receipt(&root, "narrow", &source, &source, &base, &one, false);
        let error = adoption(&root, &source, &all_receipts(&root).unwrap()).unwrap_err();
        assert!(
            error.to_string().contains("SUSI_LOCAL_GATE=full"),
            "{error}"
        );
        git(
            &root,
            &["update-ref", "-d", &format!("{RECEIPTS}narrow/{source}")],
        )
        .unwrap();
        assert!(evidence_rooted(&root, &narrowed).unwrap());

        let whole = receipt(&root, "whole", &source, &source, &base, &one, true);
        let found = adoption(&root, &source, &all_receipts(&root).unwrap()).unwrap();
        assert_eq!(found.receipts, vec![whole]);
        assert_eq!(found.baseline, base);
        assert_eq!(found.coordinator, one);
        assert_eq!(found.workers, vec!["whole".to_owned()]);

        receipt(&root, "other", &base, &source, &base, &"2".repeat(64), true);
        let error = adoption(&root, &source, &all_receipts(&root).unwrap()).unwrap_err();
        assert!(
            error.to_string().contains("different coordinators"),
            "{error}"
        );
    }

    #[test]
    fn coordinator_unrooted_receipt_is_refused() {
        let root = repo("unrooted");
        let base = commit_file(&root, "a.txt", "a");
        let source = commit_file(&root, "src/b.txt", "b");
        let object = receipt(&root, "w", &source, &source, &base, &"1".repeat(64), true);
        git(&root, &["update-ref", "-d", &format!("{EVIDENCE}{object}")]).unwrap();
        let error = adoption(&root, &source, &all_receipts(&root).unwrap()).unwrap_err();
        assert!(error.to_string().contains("durable evidence"), "{error}");
    }

    #[test]
    fn coordinator_snapshots_the_verifier_by_digest_and_never_substitutes() {
        let home = scratch("verifier");
        let elsewhere = scratch("verifier-source").join("local-workflow.bin");
        fs::write(&elsewhere, b"integrating binary").unwrap();
        let want = digest(b"integrating binary");
        let held = pinned_verifier(&home, std::slice::from_ref(&elsewhere), None, &want).unwrap();
        assert!(held.starts_with(home.join("verifiers")));
        assert_eq!(file_digest(&held).unwrap(), want);
        // Held copy is found again without the original.
        fs::remove_file(&elsewhere).unwrap();
        assert_eq!(pinned_verifier(&home, &[], None, &want).unwrap(), held);
        let error = pinned_verifier(&home, &[], None, &"9".repeat(64)).unwrap_err();
        assert!(error.to_string().contains("not snapshotted"), "{error}");
        let error = pinned_verifier(&home, &[], Some(&held), &"9".repeat(64)).unwrap_err();
        assert!(error.to_string().contains("coordinator digest"), "{error}");
    }

    #[test]
    fn coordinator_builds_tools_only_from_a_released_ancestor() {
        let root = repo("tools");
        let released = commit_file(&root, "crates/susi-unsafe-ratchet/Cargo.toml", "[package]");
        git(&root, &["tag", "v0.0.1"]).unwrap();
        let head = commit_file(&root, "src/c.txt", "c");
        assert_eq!(
            tools_source(&root, None, &head).unwrap(),
            ("v0.0.1".to_owned(), released.clone())
        );
        assert!(tools_source(&root, Some(&head), &head).is_err());
        let unrelated = repo("tools-orphan");
        let orphan = commit_file(&unrelated, "crates/susi-unsafe-ratchet/Cargo.toml", "x");
        git(&root, &["fetch", "-q", text(&unrelated).unwrap(), &orphan]).unwrap();
        assert!(tools_source(&root, Some(&orphan), &head).is_err());
        let bare = repo("tools-none");
        let only = commit_file(&bare, "a.txt", "a");
        assert!(tools_source(&bare, None, &only).is_err());
    }

    #[test]
    fn coordinator_held_tools_detect_tampering() {
        let home = scratch("held");
        let tool = home.join("unsafe_measure");
        fs::write(&tool, b"tool").unwrap();
        let other = home.join("unsafe_ratchet");
        fs::write(&other, b"other").unwrap();
        let record = home.join("build.json");
        fs::write(
            &record,
            serde_json::to_vec(&json!({"tools":{
                "unsafe_measure":{"path":text(&tool).unwrap(),"sha256":digest(b"tool")},
                "unsafe_ratchet":{"path":text(&other).unwrap(),"sha256":digest(b"other")}}}))
            .unwrap(),
        )
        .unwrap();
        assert!(held_tools(&record).unwrap().is_some());
        fs::write(&tool, b"swapped").unwrap();
        assert!(held_tools(&record).is_err());
        assert!(held_tools(&home.join("missing.json")).unwrap().is_none());
    }

    #[test]
    fn coordinator_state_must_live_outside_the_repository() {
        let root = repo("outside");
        let inside = root.join("target").join("coordinator");
        fs::create_dir_all(&inside).unwrap();
        assert!(require_outside(&inside, &[&root]).is_err());
        let outside = scratch("outside-home");
        require_outside(&outside, &[&root]).unwrap();
    }

    #[test]
    fn coordinator_diagnostics_list_unreceipted_commits_since_the_release() {
        let root = repo("diagnostics");
        let release = commit_file(&root, "a.txt", "a");
        let unowned = commit_file(&root, "b.txt", "b");
        let base = commit_file(&root, "c.txt", "c");
        let source = commit_file(&root, "src/d.txt", "d");
        receipt(&root, "w", &source, &source, &base, &"1".repeat(64), true);
        let receipts = all_receipts(&root).unwrap();
        let found = adoption(&root, &source, &receipts).unwrap();
        let bytes = diagnostics(&root, &source, &found, &receipts, Some(&release)).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["historical_ownership"], "incomplete");
        assert_eq!(value["baseline"]["commit"], base);
        let listed: Vec<&str> = value["unreceipted_since_last_promoted_release"]["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(listed, vec![base.as_str(), unowned.as_str()]);
    }

    #[test]
    fn coordinator_arguments_are_strict() {
        assert!(parse(&["--head".into()]).is_err());
        assert!(parse(&["--bogus".into(), "x".into()]).is_err());
        assert!(parse(&["--head".into(), "a".into(), "--head".into(), "b".into()]).is_err());
        let options = parse(&["--tools-source".into(), "v1".into()]).unwrap();
        assert_eq!(options.tools_source.as_deref(), Some("v1"));
    }
}
