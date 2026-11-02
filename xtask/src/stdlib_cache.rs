use super::{artifacts, git, reject_redirects, Result, TARGET};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

// The local Git control plane owns the admission refs, just as it owns integration
// refs. Cache files, builder receipts and caller-provided paths have no authority.
fn reference(key: &str) -> String {
    format!("refs/susi/stdlib-admissions/v1/{key}")
}

fn blob_id(root: &Path, bytes: &[u8], write: bool) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(root)
        .arg("hash-object")
        .args(write.then_some("-w"))
        .arg("--stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing git input")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err("could not persist stdlib admission".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub(super) fn publish(root: &Path, key: &str, record: &Value) -> Result<()> {
    let oid = blob_id(root, &serde_json::to_vec(record)?, true)?;
    let name = reference(key);
    let zero = "0".repeat(oid.len());
    // A concurrent builder may have qualified different bytes for the same
    // inputs. Keep the first immutable admission, never overwrite it.
    if git(root, &["update-ref", &name, &oid, &zero]).is_err()
        && git(root, &["rev-parse", "--verify", &name]).is_err()
    {
        return Err("could not publish stdlib admission".into());
    }
    Ok(())
}

pub(super) fn lookup(root: &Path, storage: &Path, key: &str) -> Result<Option<(PathBuf, Value)>> {
    let name = reference(key);
    let status = Command::new("git")
        .current_dir(root)
        .args(["show-ref", "--verify", "--quiet", &name])
        .status()?;
    if status.code() == Some(1) {
        return Ok(None);
    }
    if !status.success() {
        return Err("could not read stdlib admission".into());
    }
    let oid = git(root, &["rev-parse", "--verify", &name])?;
    let bytes = Command::new("git")
        .current_dir(root)
        .args(["--no-replace-objects", "cat-file", "blob", &oid])
        .output()?;
    if !bytes.status.success() {
        return Err("missing stdlib admission blob".into());
    }
    if blob_id(root, &bytes.stdout, false)? != oid {
        return Err("stdlib admission blob failed content verification".into());
    }
    let record: Value = serde_json::from_slice(&bytes.stdout)?;
    validate_identity(&record, key)?;
    let digest = record["object"].as_str().ok_or("missing stdlib object")?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid stdlib object digest".into());
    }
    let dir = storage.join("objects").join(digest);
    reject_redirects(&dir)?;
    validate_layout(&dir)?;
    if artifacts(&dir)? != record["artifacts"] {
        return Err("cached stdlib archives differ from admission".into());
    }
    Ok(Some((dir, record)))
}

fn validate_identity(record: &Value, key: &str) -> Result<()> {
    if record["schema"] != "susi/executed-sysroot/v3"
        || record["identity"] != key
        || record["object"]
            != super::super::sysroot_sources::hash(&serde_json::to_vec(&record["artifacts"])?)
    {
        return Err("stdlib admission identity or inventory mismatch".into());
    }
    Ok(())
}

pub(super) fn validate_layout(dir: &Path) -> Result<()> {
    let mut parent = dir.join("sysroot");
    for name in ["lib", "rustlib", TARGET, "lib"] {
        reject_redirects(&parent)?;
        let entries = fs::read_dir(&parent)?.collect::<std::io::Result<Vec<_>>>()?;
        if entries.len() != 1 || entries[0].file_name() != name || !entries[0].file_type()?.is_dir()
        {
            return Err("unexpected compiler-visible sysroot entry".into());
        }
        parent.push(name);
    }
    reject_redirects(&parent)?;
    Ok(())
}

pub(super) fn store(
    storage: &Path,
    source: &Path,
    inventory: &Value,
    nonce: u128,
) -> Result<String> {
    let digest = super::super::sysroot_sources::hash(&serde_json::to_vec(inventory)?);
    let objects = storage.join("objects");
    fs::create_dir_all(&objects)?;
    reject_redirects(&objects)?;
    let target = objects.join(&digest);
    if !target.exists() {
        let stage = objects.join(format!("tmp-{}-{nonce}", std::process::id()));
        fs::create_dir(&stage)?;
        let lib = Path::new("sysroot/lib/rustlib").join(TARGET).join("lib");
        fs::create_dir_all(stage.join(&lib))?;
        for entry in fs::read_dir(source.join(&lib))? {
            let entry = entry?;
            fs::copy(entry.path(), stage.join(&lib).join(entry.file_name()))?;
        }
        if artifacts(&stage)? != *inventory {
            return Err("stdlib changed during cache publication".into());
        }
        if let Err(error) = fs::rename(&stage, &target) {
            if !target.is_dir() {
                return Err(error.into());
            }
            fs::remove_dir_all(stage)?;
        }
    }
    validate_layout(&target)?;
    if artifacts(&target)? != *inventory {
        return Err("existing stdlib object corrupted".into());
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_the_admitted_input_and_exact_archive_set_can_be_reused() {
        let scratch = std::env::temp_dir().join(format!(
            "stdlib-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&scratch).unwrap();
        assert!(Command::new("git")
            .current_dir(&scratch)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        let storage = scratch.join("store");
        let source = scratch.join("build");
        let lib = Path::new("sysroot/lib/rustlib").join(TARGET).join("lib");
        fs::create_dir_all(source.join(&lib)).unwrap();
        for index in 0..20 {
            fs::write(
                source.join(&lib).join(format!("lib{index}.rlib")),
                index.to_string(),
            )
            .unwrap();
        }
        let expected = artifacts(&source).unwrap();
        let object = store(&storage, &source, &expected, 1).unwrap();
        let key = "a".repeat(64);
        let record = json!({"schema":"susi/executed-sysroot/v3","identity":key,"object":object,"artifacts":expected});
        let dir = storage.join("objects").join(&object);
        fs::write(
            dir.join("executed-record.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(lookup(&scratch, &storage, &key).unwrap().is_none());
        publish(&scratch, &key, &record).unwrap();
        assert_eq!(lookup(&scratch, &storage, &key).unwrap().unwrap().1, record);
        let other = "b".repeat(64);
        assert!(lookup(&scratch, &storage, &other).unwrap().is_none());
        assert!(validate_identity(&record, &other).is_err());
        let mut rival = record.clone();
        rival["different"] = json!(true);
        publish(&scratch, &key, &rival).unwrap();
        assert_eq!(lookup(&scratch, &storage, &key).unwrap().unwrap().1, record);
        let extra = dir.join("sysroot/lib/unapproved");
        fs::write(&extra, b"extra").unwrap();
        assert!(lookup(&scratch, &storage, &key).is_err());
        fs::remove_file(extra).unwrap();
        fs::write(dir.join(&lib).join("lib0.rlib"), b"replacement").unwrap();
        let mut forged = record.clone();
        forged["artifacts"] = artifacts(&dir).unwrap();
        fs::write(
            dir.join("executed-record.json"),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        assert!(lookup(&scratch, &storage, &key).is_err());
        fs::remove_dir_all(scratch).unwrap();
    }
}
