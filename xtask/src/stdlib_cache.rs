use super::{artifacts, reject_redirects, Result, TARGET};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Input identity and SHA-256 of an independently observed qualified build
/// record. Unrecognized inputs build normally. A cache file or Git ref cannot
/// add a pin. The record binds the exact archive hashes, source and notices.
const REVIEWED: &str = include_str!("reviewed-stdlib.json");

pub(super) fn publish(storage: &Path, record: &Value, nonce: u128) -> Result<String> {
    let bytes = serde_json::to_vec(record)?;
    let digest = super::super::sysroot_sources::hash(&bytes);
    let records = storage.join("records");
    fs::create_dir_all(&records)?;
    reject_redirects(&records)?;
    let dest = records.join(&digest);
    if !dest.exists() {
        let tmp = records.join(format!("tmp-{}-{nonce}", std::process::id()));
        fs::write(&tmp, &bytes)?;
        // Link creation atomically publishes without overwriting a racing writer.
        let link = fs::hard_link(&tmp, &dest);
        fs::remove_file(tmp)?;
        if let Err(error) = link {
            if !dest.is_file() {
                return Err(error.into());
            }
        }
    }
    reject_redirects(&dest)?;
    if fs::read(&dest)? != bytes {
        return Err("qualified stdlib record was modified".into());
    }
    Ok(digest)
}

pub(super) fn lookup(storage: &Path, key: &str) -> Result<Option<(PathBuf, Value)>> {
    let pins: Vec<(&str, &str)> = serde_json::from_str(REVIEWED)?;
    lookup_reviewed(storage, key, &pins)
}

fn lookup_reviewed(
    storage: &Path,
    key: &str,
    pins: &[(&str, &str)],
) -> Result<Option<(PathBuf, Value)>> {
    let Some((_, digest)) = pins.iter().find(|(input, _)| *input == key) else {
        return Ok(None);
    };
    let record_file = storage.join("records").join(digest);
    match fs::symlink_metadata(&record_file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => reject_redirects(&record_file)?,
    }
    let bytes = fs::read(record_file)?;
    if super::super::sysroot_sources::hash(&bytes) != *digest {
        return Err("stdlib record differs from reviewed SHA-256 pin".into());
    }
    let record: Value = serde_json::from_slice(&bytes)?;
    validate_identity(&record, key)?;
    let object = record["object"].as_str().ok_or("missing stdlib object")?;
    let dir = storage.join("objects").join(object);
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

#[cfg(windows)]
pub(super) fn namespace_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut path = dir.to_path_buf();
    let mut dirs = vec![path.clone()];
    for name in ["sysroot", "lib", "rustlib", TARGET, "lib"] {
        path.push(name);
        dirs.push(path.clone());
    }
    dirs
}

#[cfg(windows)]
pub(super) fn anchor(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let file = fs::OpenOptions::new()
        .access_mode(0x80)
        .share_mode(3)
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 {
        return Err("redirected namespace ancestor".into());
    }
    Ok(file)
}
pub(super) fn hold_namespace(dir: &Path) -> Result<Vec<fs::File>> {
    #[cfg(windows)]
    {
        let mut guards = Vec::new();
        for ancestor in dir.ancestors().skip(1) {
            guards.push(anchor(ancestor)?);
        }
        for path in namespace_dirs(dir) {
            reject_redirects(&path)?;
            guards.push(susi_vendor_winapi::sealed_directory::hold(&path)?);
        }
        Ok(guards)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Err("only the reviewed Windows filesystem namespace can be reused".into())
    }
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
        #[cfg(windows)]
        for subdir in namespace_dirs(&stage).into_iter().rev() {
            reject_redirects(&subdir)?;
            susi_vendor_winapi::sealed_directory::seal_new(&subdir)?;
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
            storage.join("executed-record.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(lookup(&storage, &key).unwrap().is_none());
        let pin = publish(&storage, &record, 2).unwrap();
        let pins = [(key.as_str(), pin.as_str())];
        assert_eq!(
            lookup_reviewed(&storage, &key, &pins).unwrap().unwrap().1,
            record
        );
        let other = "b".repeat(64);
        assert!(lookup_reviewed(&storage, &other, &pins).unwrap().is_none());
        assert!(validate_identity(&record, &other).is_err());
        let mut rival = record.clone();
        rival["different"] = json!(true);
        publish(&storage, &rival, 3).unwrap();
        assert_eq!(
            lookup_reviewed(&storage, &key, &pins).unwrap().unwrap().1,
            record
        );
        let trusted_record = storage.join("records").join(&pin);
        let original = fs::read(&trusted_record).unwrap();
        fs::write(&trusted_record, serde_json::to_vec(&rival).unwrap()).unwrap();
        assert!(lookup_reviewed(&storage, &key, &pins).is_err());
        fs::write(&trusted_record, original).unwrap();
        let extra = dir.join("sysroot/lib/unapproved");
        #[cfg(windows)]
        {
            let live = hold_namespace(&dir).unwrap();
            assert_eq!(
                fs::write(&extra, b"extra").unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert!(fs::rename(dir.parent().unwrap(), storage.join("swapped")).is_err());
            drop(live);
        }
        #[cfg(not(windows))]
        {
            fs::write(&extra, b"extra").unwrap();
            assert!(lookup_reviewed(&storage, &key, &pins).is_err());
            fs::remove_file(extra).unwrap();
        }
        fs::write(dir.join(&lib).join("lib0.rlib"), b"replacement").unwrap();
        let mut forged = record.clone();
        forged["artifacts"] = artifacts(&dir).unwrap();
        fs::write(
            storage.join("executed-record.json"),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        assert!(lookup_reviewed(&storage, &key, &pins).is_err());
        fs::remove_dir_all(scratch).unwrap();
    }
}
