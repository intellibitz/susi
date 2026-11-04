//! A single checkpoint admitted only by a detached signature from an independent
//! authority. Run this executable from protected source. The subject is Git
//! data, never code to check out or execute. Legacy task checks are separate.
#![forbid(unsafe_code)]
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::Write,
    path::Path,
    process::{Command, ExitCode, Stdio},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const REPO: &str = "intellibitz/susi";
const BRANCH: &str = "codex/step1-qualified-0.21.43";
const BASE: &str = "e6824c957b1c41e5c2611266da564f99bf620394";
const HEAD: &str = "216e3bb157b677827757723da6c5126cebdc63a5";
const TREE: &str = "540c7bcdeb0be389eb167b71fcef4817f2b52802";
const PREVIOUS: &str = "bdccb2c75841bc9e810b9f6301d8f08fbf599d28";
const SOURCE: &str = "d1f8d1fb82d151a2a65698d7f80e693e18f3c56c";
const RECEIPT: &str = "3c1f7e105f15bc1e7efb2fc8d588fe842ebfb2ba";
const RECEIPT_SHA: &str = "d3237a61ee068e827f609593048caea9ba933b0046b5da73773e386c03932a44";
const PUBLICATION_SHA: &str = "33a9b59baa83e95061b5b910cee2ca4f4d1e0d93a98cd489d5b5f3d1dc199102";

// Observed receipt identifiers keyed by worker source, not by intermediate merge.
const ISSUED: &[(&str, &str, &str, &str)] = &[
    (
        "d613f275913aadf25c6a13e5eff26a2917a897cf",
        "4cd59dc765f6009c103c6acc566f7a4615bccb8f",
        "3c71fd282a17aba17d3e781a52d3dad25577fe59",
        "step1-c-evidence",
    ),
    (
        "31a54366400767981f7014d7e5785ae551fa54ba",
        "86404da6d9e50a9e510b6c38e68f90c1efa7d28b",
        "9c3def2a18e2b19bb67fd0d68e43980c85688632",
        "step1-version-path",
    ),
    (
        "6875c5ca1f4e0962d8c57b0e1c23c655721f3bc6",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "d5cf2748eb35e3891279ac141ae1cd2cc704f190",
        "step1-batch-1102",
    ),
    (
        "5a2f846d6a7687872356f270393a9b3c226c2a63",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "265dd8b15fb5af6a7c65b49ae024f51e88feccb2",
        "count1102",
    ),
    (
        "4d4569e406fefa9449b69a4dcb3b92d813bde4fe",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "6f095ebe0f15a10fd8a6c688f852346a4867d6bd",
        "journeylock-1102",
    ),
    (
        "02c5da9e1cea9577cfe5ebe503c32fb9df008593",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "b3677b45bbf78fe0898abda541a633009b49892a",
        "channelguard-1102",
    ),
    (
        "880bde4d4093abc2628190865788156dccdf477d",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "a796ffcbe1e40506af8655110d3a0311218e349e",
        "launchfix41",
    ),
    (
        "12291209246e297a52d3ad43952b1458a3b8fd80",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "ec708bb89a20704245ee4602c189fc4df2a1b148",
        "core-quote-1102",
    ),
    (
        "9a8522bbfdad002b24ddaae46131fc996a64ff1c",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "d996c1faeef0042f5df117e238b30d60cdfeb449",
        "change-live-1102",
    ),
    (
        "1289171033be211390ada0d8a7fc958086917241",
        "bf3fde3e88b5b6d2039442220896586e620caf52",
        "f66c46a37f2f73695946fd9b30e825ae688bfaf3",
        "ceo-speed-standard",
    ),
    (
        "918a51cb9330a9ce574c8c56191a8aa607c1c7c8",
        "45285cdcd6399e64b44ce8f7e2f650e267002a52",
        "537e64b3f36029ed0f109a9770eeb3a5611a361c",
        "version42-1102",
    ),
    (
        "a55f3b1025d869dd049129c52148031134bd9927",
        "10725a631f2967947da0b29462d469a054d5f0d6",
        "b9b755085cf8feb4da624f3935b038ff0e36a228",
        "modelread1102",
    ),
    (
        "68586630e01cca4bec4a1afd2b077bfdc5040c41",
        "10725a631f2967947da0b29462d469a054d5f0d6",
        "f369f7ced5de5508f0d5ab23febc7f08dc439c3b",
        "grantstorage-1102",
    ),
    (
        "c31016b9cd58d5ecf5587dd1eddf606df8ba9618",
        "10725a631f2967947da0b29462d469a054d5f0d6",
        "bb7ca02876e024a5dce888477313b974cb627473",
        "nativecleanup-1102",
    ),
    (
        "b8b9a37dbd9ea278c9cc20dda396c72a48d95da0",
        "10725a631f2967947da0b29462d469a054d5f0d6",
        "01e7e7cc9a6cb822d054ab8efa376783cd689592",
        "release42-proof",
    ),
    (
        "db275a9f1cb152250a7f7e3dea328a4937348d09",
        "bdccb2c75841bc9e810b9f6301d8f08fbf599d28",
        "59760847517cb1ff51bc59233737b2b8d8a4fa96",
        "version43-1102",
    ),
    (
        "d1f8d1fb82d151a2a65698d7f80e693e18f3c56c",
        "216e3bb157b677827757723da6c5126cebdc63a5",
        "3c1f7e105f15bc1e7efb2fc8d588fe842ebfb2ba",
        "release43-proof",
    ),
];

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("--no-replace-objects")
        .args(args)
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        return Err(format!("Git object inspection failed: {args:?}").into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn paths(repo: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    Ok(git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-only",
            "-z",
            base,
            head,
            "--",
        ],
    )?
    .split_terminator('\0')
    .map(str::to_owned)
    .collect())
}

fn statement(
    repo: &Path,
    receipt: &[u8],
    publication: &[u8],
    context: &[String],
) -> Result<Vec<u8>> {
    if context != [REPO, "1299", "main", BRANCH, BASE, HEAD] {
        return Err(
            "event differs from the admitted repository, PR, branches, base or head".into(),
        );
    }
    if git(repo, &["rev-parse", &format!("{HEAD}^{{tree}}")])?.trim() != TREE
        || git(repo, &["merge-base", BASE, HEAD])?.trim() != BASE
        || git(repo, &["show", "-s", "--format=%P", HEAD])?.trim() != format!("{PREVIOUS} {SOURCE}")
    {
        return Err("checkpoint tree or ancestry mismatch".into());
    }
    if digest(receipt) != RECEIPT_SHA || digest(publication) != PUBLICATION_SHA {
        return Err("native acceptance receipt or external publication readback mismatch".into());
    }
    let proof: Value = serde_json::from_slice(publication)?;
    if proof["repository"] != REPO
        || proof["tip"] != HEAD
        || proof["tree"] != TREE
        || proof["remote_main_before_and_after"] != BASE
        || proof["official_receipt"] != RECEIPT
    {
        return Err("guard readback does not bind this checkpoint".into());
    }
    let text = std::str::from_utf8(receipt)?;
    let gates: Vec<_> = text
        .lines()
        .take_while(|line| !line.starts_with("sysroot="))
        .filter(|line| line.contains(" => "))
        .collect();
    if gates.len() != 14
        || !gates.iter().all(|s| s.contains(" => exit code: 0;"))
        || !gates.iter().any(|s| {
            s.starts_with("cargo test --workspace ") && s.ends_with("observed-passes=4242")
        })
    {
        return Err("expected successful native gate records missing".into());
    }
    // The base is an ancestor via a side branch. Preserve every first-parent
    // integration delta as well as the net PR scope; never equate the last
    // worker's three paths with the entire checkpoint's ownership.
    let mut integrations = Vec::new();
    for merge in git(
        repo,
        &[
            "rev-list",
            "--reverse",
            "--first-parent",
            &format!("{BASE}..{HEAD}"),
        ],
    )?
    .lines()
    {
        let parents = git(repo, &["show", "-s", "--format=%P", merge])?;
        let p: Vec<_> = parents.split_whitespace().collect();
        if p.len() != 2 {
            return Err("unexpected checkpoint provenance shape".into());
        }
        let (_, tested, receipt_id, worker) = ISSUED
            .iter()
            .find(|row| row.0 == p[1])
            .ok_or("unrecognized worker source")?;
        integrations.push(json!({"merge":merge,"kind":if merge==*tested {"receipted_tree"} else {"intermediate_batch_assembly"},
            "tested_commit":tested,"worker_receipt":receipt_id,"worker":worker,"previous":p[0],"source":p[1],"paths":paths(repo,p[0],merge)?}));
    }
    let scope = paths(repo, PREVIOUS, HEAD)?;
    if scope
        != [
            ".agents/evidence/release43-windows.json",
            ".agents/tasks/T-RELEASE43PROOF-1.json",
            "docs/step1-acceptance.md",
        ]
    {
        return Err("final worker scope mismatch".into());
    }
    let mut bytes = serde_json::to_vec_pretty(
        &json!({"schema":"susi/checkpoint-compliance/v2","gate_scope":"Native Windows integration only. No attestation of the Linux unsafe, license or advisory gates.",
        "repository":REPO,"pr":1299,"base_branch":"main","head_branch":BRANCH,"base":BASE,"head":HEAD,"tree":TREE,
        "receipt":RECEIPT,"receipt_sha256":RECEIPT_SHA,"guard_publication_sha256":PUBLICATION_SHA,
        "gates":gates,"paths":paths(repo,BASE,HEAD)?,"integrations":integrations,
        "final_worker":{"name":"release43-proof","task":"T-RELEASE43PROOF-1","paths":scope}}),
    )?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn verify_signature(bytes: &[u8], signature: &Path, keyring: &Path, primary: &str) -> Result<()> {
    if !matches!(primary.len(), 40 | 64) || !primary.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid pinned primary fingerprint".into());
    }
    let mut child = Command::new("gpgv")
        .args(["--status-fd", "1", "--keyring"])
        .arg(keyring.canonicalize()?)
        .arg("--")
        .arg(signature.canonicalize()?)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing verification input")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    let status = std::str::from_utf8(&output.stdout)?;
    let valid: Vec<_> = status
        .lines()
        .filter_map(|s| s.strip_prefix("[GNUPG:] VALIDSIG "))
        .collect();
    let signer = match valid.as_slice() {
        [one] => one.split_whitespace().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    // GnuPG VALIDSIG fields 7/8 are the digest algorithm and signature
    // class; 9 identifies the primary key even if a signing subkey was used.
    if !output.status.success()
        || !matches!(signer.get(7), Some(&"8" | &"9" | &"10"))
        || signer.get(8) != Some(&"00")
        || signer.get(9) != Some(&primary)
        || status.lines().any(|line| {
            ["EXPKEYSIG", "EXPSIG", "REVKEYSIG", "BADSIG", "ERRSIG"]
                .iter()
                .any(|code| line.starts_with(&format!("[GNUPG:] {code} ")))
        })
    {
        return Err("trusted detached OpenPGP signature required".into());
    }
    Ok(())
}

fn run(args: &[String]) -> Result<()> {
    if args.len() < 11 {
        return Err("expected MODE subject-repo receipt publication statement repository pr base-branch head-branch base head [signature keyring primary-fingerprint]".into());
    }
    let bytes = statement(
        Path::new(&args[1]),
        &fs::read(&args[2])?,
        &fs::read(&args[3])?,
        &args[5..11],
    )?;
    match (args[0].as_str(), args.len()) {
        ("statement", 11) => fs::write(&args[4], bytes)?,
        ("verify", 14) => {
            if fs::read(&args[4])? != bytes {
                return Err("attestation differs from reconstructed checkpoint".into());
            }
            verify_signature(
                &bytes,
                Path::new(&args[11]),
                Path::new(&args[12]),
                &args[13],
            )?;
            println!(
                "Verified PR 1299, head {HEAD}, tree {TREE}, statement SHA-256 {}",
                digest(&bytes)
            );
        }
        _ => return Err("unsupported command or arguments".into()),
    }
    Ok(())
}
fn main() -> ExitCode {
    match run(&env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("checkpoint compliance refused: {e}");
            ExitCode::FAILURE
        }
    }
}
