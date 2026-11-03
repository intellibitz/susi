//! Register and verify the ordinary per-user Windows launch bindings.
mod shell_link;
use std::fs;
use std::io::{self, Read, Write};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};
use windows_registry::{Key, Type, CURRENT_USER, LOCAL_MACHINE};

fn safe_directory(path: &Path) -> io::Result<()> {
    for part in path.ancestors() {
        let meta = fs::symlink_metadata(part)?;
        if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other(format!(
                "redirected shell directory: {}",
                part.display()
            )));
        }
    }
    Ok(())
}

fn file_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !meta.is_file() || meta.file_attributes() & 0x400 != 0 {
        return Err(io::Error::other("unsafe shell registration file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::other("shell registration file exceeds one MiB"));
    }
    Ok(Some(bytes))
}

fn expand(raw: &str) -> String {
    let mut expanded = String::new();
    let mut tail = raw;
    while let Some((prefix, remaining)) = tail.split_once('%') {
        expanded.push_str(prefix);
        let Some((key, next)) = remaining.split_once('%') else {
            expanded.push('%');
            expanded.push_str(remaining);
            return expanded;
        };
        let replacement = (!key.is_empty() && !key.contains(['=', '\0']))
            .then(|| std::env::var(key).ok())
            .flatten();
        if let Some(value) = replacement {
            expanded.push_str(&value);
        } else {
            expanded.push('%');
            expanded.push_str(key);
            expanded.push('%');
        }
        tail = next;
    }
    expanded.push_str(tail);
    expanded
}

fn programs() -> io::Result<PathBuf> {
    for branch in ["User Shell Folders", "Shell Folders"] {
        let result = CURRENT_USER
            .open(format!(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\{branch}"
            ))
            .and_then(|k| k.get_string("Programs"));
        match result {
            Ok(raw) => {
                let path = PathBuf::from(expand(&raw));
                if !path.is_absolute() {
                    return Err(io::Error::other(
                        "Start Menu Programs directory is not absolute",
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.code().0 == 0x80070002u32 as i32 => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| io::Error::other("roaming profile directory is missing"))?;
    let path = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs");
    if !path.is_absolute() {
        return Err(io::Error::other(
            "roaming profile directory is not absolute",
        ));
    }
    Ok(path)
}

fn user_path(key: &Key) -> io::Result<(String, Type)> {
    match key.get_value("Path") {
        Ok(value) => {
            let ty = value.ty();
            Ok((String::try_from(value).map_err(io::Error::other)?, ty))
        }
        Err(error) if error.code().0 == 0x80070002u32 as i32 => {
            Ok((String::new(), Type::ExpandString))
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

fn add_path(original: &str, entry: &str) -> String {
    let same = |s: &str| {
        s.trim_matches('"')
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(entry)
    };
    if original.split(';').next().is_some_and(same) {
        return original.into();
    }
    std::iter::once(entry)
        .chain(
            original
                .split(';')
                .filter(|s| !same(s) && !original.is_empty()),
        )
        .collect::<Vec<_>>()
        .join(";")
}

fn machine_path(entry: &Path) -> io::Result<()> {
    let machine = LOCAL_MACHINE
        .open(r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment")
        .map_err(io::Error::other)?;
    let raw = user_path(&machine)?.0;
    for dir in std::env::split_paths(&expand(&raw)).filter(|d| d.is_absolute()) {
        let candidate = dir.join("susi.exe");
        if candidate.is_file() {
            if candidate != entry {
                return Err(io::Error::other(format!(
                    "machine PATH selects {} before the per-user installation",
                    candidate.display()
                )));
            }
            break;
        }
    }
    Ok(())
}

fn retain_legacy(root: &Path, shortcut: &[u8]) -> io::Result<()> {
    let target = shell_link::legacy_local_target(shortcut)?;
    let channel = file_bytes(&root.join("channel.json"))?
        .ok_or_else(|| io::Error::other("refusing to replace an unrelated Start Menu shortcut"))?;
    let record: serde_json::Value = serde_json::from_slice(&channel)?;
    let sha = record["sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::other("missing legacy release digest"))?;
    let expected = root.join("builds").join(sha).join("susi.exe");
    if record["current"].as_str().map(Path::new) != Some(&expected) || target != expected {
        return Err(io::Error::other(
            "existing shortcut does not select the verified legacy release",
        ));
    }
    let backup = root.join("original-production-shortcut.lnk");
    match file_bytes(&backup)? {
        Some(bytes) if bytes != shortcut => {
            return Err(io::Error::other(
                "existing shortcut backup differs; preserving both records",
            ))
        }
        Some(_) => {}
        None => {
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(backup)?;
            out.write_all(shortcut)?;
            out.sync_all()?;
        }
    }
    Ok(())
}

fn register_at(root: &Path, programs: &Path, key: &Key) -> io::Result<bool> {
    safe_directory(root)?;
    safe_directory(programs)?;
    let entry = root.join("entry");
    safe_directory(&entry)?;
    let work = root.join("workspace");
    if !work.try_exists()? {
        fs::create_dir(&work)?;
    }
    safe_directory(&work)?;
    let desired = shell_link::encode(&entry.join("susi.exe"), &work)?;
    let shortcut = programs.join("Susi (local production).lnk");
    let old = file_bytes(&shortcut)?;
    let (raw, ty) = user_path(key)?;
    let updated = add_path(
        &raw,
        entry
            .to_str()
            .ok_or_else(|| io::Error::other("invalid entry path"))?,
    );
    if old.as_deref() != Some(&desired) {
        // Susi's own earlier link (it launched the retired `susi shell`) is
        // regenerable, so it is replaced without a legacy backup.
        let retired = shell_link::retired_shell_encoding(&entry.join("susi.exe"), &work)?;
        if let Some(bytes) = old.filter(|bytes| *bytes != retired) {
            retain_legacy(root, &bytes)?;
        }
        let pending = programs.join(".susi-production-link-pending.tmp");
        if file_bytes(&pending)?.is_some() {
            fs::remove_file(&pending)?;
        }
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)?;
        out.write_all(&desired)?;
        out.sync_all()?;
        drop(out);
        fs::rename(pending, &shortcut)?;
    }
    let notification = root.join("path-notification-pending");
    let pending = file_bytes(&notification)?;
    if pending.as_ref().is_some_and(|bytes| !bytes.is_empty()) {
        return Err(io::Error::other("invalid environment-notification journal"));
    }
    if updated != raw {
        // Persist intent before changing HKCU. If the caller dies between the
        // registry write and notification, retry even though PATH already matches.
        if pending.is_none() {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&notification)?
                .sync_all()?;
        }
        match ty {
            Type::String => key.set_string("Path", &updated),
            Type::ExpandString => key.set_expand_string("Path", &updated),
            Type::U32 | Type::U64 | Type::MultiString | Type::Bytes | Type::Other(_) => {
                return Err(io::Error::other("user PATH must be a string"))
            }
        }
        .map_err(io::Error::other)?;
    }
    if file_bytes(&shortcut)?.as_deref() != Some(&desired) || user_path(key)?.0 != updated {
        return Err(io::Error::other(
            "ordinary launch binding verification failed",
        ));
    }
    Ok(updated != raw || pending.is_some())
}

pub fn register(root: &Path) -> EaiResult<bool> {
    let write = || -> io::Result<bool> {
        machine_path(&root.join("entry/susi.exe"))?;
        let key = CURRENT_USER
            .create("Environment")
            .map_err(io::Error::other)?;
        register_at(root, &programs()?, &key)
    };
    write().map_err(|e| EaiError::io(format!("register ordinary Windows launches: {e}")))
}

pub fn notification_complete(root: &Path) -> EaiResult<()> {
    fs::remove_file(root.join("path-notification-pending"))
        .map_err(|e| EaiError::io(format!("finish environment notification: {e}")))
}

pub fn verify(root: &Path) -> EaiResult<()> {
    let inspect = || -> io::Result<()> {
        let entry = root.join("entry");
        safe_directory(&entry)?;
        machine_path(&entry.join("susi.exe"))?;
        let key = CURRENT_USER.open("Environment").map_err(io::Error::other)?;
        let raw = user_path(&key)?.0;
        if raw.split(';').next().is_none_or(|s| {
            !s.trim_matches('"')
                .eq_ignore_ascii_case(entry.to_str().unwrap_or_default())
        }) {
            return Err(io::Error::other(
                "user PATH does not start with the verified entry",
            ));
        }
        let expected = shell_link::encode(&entry.join("susi.exe"), &root.join("workspace"))?;
        if file_bytes(&programs()?.join("Susi (local production).lnk"))?.as_deref()
            != Some(&expected)
        {
            return Err(io::Error::other(
                "Start Menu link differs from the verified entry",
            ));
        }
        Ok(())
    };
    inspect().map_err(|e| EaiError::io(format!("verify ordinary Windows launches: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_shortcut_and_expandable_user_path_are_idempotent() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("生产 Ω");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("entry")).unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            root.join("entry/susi.exe"),
        )
        .unwrap();
        let programs = scratch.path().join("Programs");
        fs::create_dir(&programs).unwrap();
        let name = format!(
            "Software\\Susi-Channel-Test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let key = CURRENT_USER.create(&name).unwrap();
        key.set_expand_string("Path", "%USERPROFILE%\\tools;C:\\other")
            .unwrap();
        assert!(register_at(&root, &programs, &key).unwrap());
        assert!(register_at(&root, &programs, &key).unwrap());
        notification_complete(&root).unwrap();
        assert!(!register_at(&root, &programs, &key).unwrap());
        let (raw, ty) = user_path(&key).unwrap();
        assert_eq!(ty, Type::ExpandString);
        assert!(raw.ends_with(";%USERPROFILE%\\tools;C:\\other"));
        assert_eq!(
            fs::read(programs.join("Susi (local production).lnk")).unwrap(),
            shell_link::encode(&root.join("entry/susi.exe"), &root.join("workspace")).unwrap()
        );
        drop(key);
        CURRENT_USER.remove_tree(name).unwrap();
    }

    #[test]
    fn retired_shell_shortcut_is_replaced_as_susis_own_link() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("production");
        fs::create_dir_all(root.join("entry")).unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            root.join("entry/susi.exe"),
        )
        .unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        let programs = scratch.path().join("Programs");
        fs::create_dir(&programs).unwrap();
        let target = root.join("entry/susi.exe");
        let work = root.join("workspace");
        let retired = shell_link::retired_shell_encoding(&target, &work).unwrap();
        let current = shell_link::encode(&target, &work).unwrap();
        assert_ne!(retired, current);
        let link = programs.join("Susi (local production).lnk");
        fs::write(&link, &retired).unwrap();
        let name = format!(
            "Software\\Susi-Channel-Retired-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let key = CURRENT_USER.create(&name).unwrap();
        key.set_expand_string("Path", "C:\\other").unwrap();
        // No channel.json exists: a foreign link would be refused here.
        register_at(&root, &programs, &key).unwrap();
        assert_eq!(fs::read(&link).unwrap(), current);
        assert!(!root.join("original-production-shortcut.lnk").exists());
        drop(key);
        CURRENT_USER.remove_tree(name).unwrap();
    }
}
