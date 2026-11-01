//! Authenticate standard-library source against independently downloaded official
//! rust-src bytes. This establishes source identity, not artifact qualification.
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io, path::Path};

pub(crate) fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn manifest() -> io::Result<BTreeMap<String, String>> {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("rust-src-1.99.0-authenticated.json"))?;
    let mut result = BTreeMap::new();
    for record in value["files"]
        .as_array()
        .ok_or_else(|| invalid("missing authenticated source files"))?
    {
        let path = record["path"]
            .as_str()
            .ok_or_else(|| invalid("invalid source path"))?;
        let digest = record["sha256"]
            .as_str()
            .ok_or_else(|| invalid("invalid source hash"))?;
        if result.insert(path.to_owned(), digest.to_owned()).is_some() {
            return Err(invalid("duplicate authenticated source"));
        }
    }
    Ok(result)
}

fn files(root: &Path, directory: &Path, output: &mut BTreeMap<String, String>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid("source reparse point is forbidden"));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(invalid("source symlink is forbidden"));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(directory)? {
            files(root, &entry?.path(), output)?;
        }
    } else if metadata.is_file() {
        let name = directory
            .strip_prefix(root)
            .map_err(|error| invalid(error.to_string()))?
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 source path"))?
            .replace('\\', "/");
        output.insert(name, hash(&fs::read(directory)?));
    } else {
        return Err(invalid("nonregular source is forbidden"));
    }
    Ok(())
}

fn verify(root: &Path, expected: &BTreeMap<String, String>) -> io::Result<()> {
    let mut actual = BTreeMap::new();
    files(root, root, &mut actual)?;
    if actual.len() != expected.len() {
        return Err(invalid(
            "standard-library source file set differs from authenticated archive",
        ));
    }
    for (path, digest) in expected {
        if actual.get(path) != Some(digest) {
            return Err(invalid(format!(
                "unauthenticated standard-library source: {path}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn original(root: &Path) -> io::Result<()> {
    verify(root, &manifest()?)
}

/// Exactly the reviewed loss of three const APIs, with no other source edits.
pub(crate) fn runtime_char_methods(original: &str) -> io::Result<String> {
    let mut text = original.replace("\r\n", "\n");
    for name in ["is_lowercase", "is_uppercase", "is_whitespace"] {
        let signature = format!("    pub const fn {name}(");
        if text.matches(&signature).count() != 1 {
            return Err(invalid("unreviewed char method signature"));
        }
        let pos = text
            .find(&signature)
            .ok_or_else(|| invalid("missing char method"))?;
        let attr = text[..pos]
            .rfind("    #[rustc_const_stable(")
            .ok_or_else(|| invalid("missing const attribute"))?;
        let end = attr
            + text[attr..]
                .find('\n')
                .ok_or_else(|| invalid("invalid const attribute"))?
            + 1;
        text.replace_range(attr..end, "");
        text = text.replacen(&signature, &format!("    pub fn {name}("), 1);
    }
    Ok(text)
}

pub(crate) fn replacement(
    root: &Path,
    authenticated_original: &Path,
    tracked_backend: &Path,
) -> io::Result<()> {
    original(authenticated_original)?;
    let mut expected = manifest()?;
    expected.insert(
        "core/src/unicode/unicode_data.rs".into(),
        hash(&fs::read(tracked_backend)?),
    );
    let methods = runtime_char_methods(&fs::read_to_string(
        authenticated_original.join("core/src/char/methods.rs"),
    )?)?;
    expected.insert("core/src/char/methods.rs".into(), hash(methods.as_bytes()));
    verify(root, &expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_manifest_requires_exact_content_and_file_set() {
        let root = std::env::temp_dir().join(format!("susi-source-auth-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rs"), "original").unwrap();
        let expected = BTreeMap::from([("a.rs".into(), hash(b"original"))]);
        verify(&root, &expected).unwrap();
        fs::write(root.join("a.rs"), "edited").unwrap();
        assert!(verify(&root, &expected).is_err());
        fs::write(root.join("a.rs"), "original").unwrap();
        fs::write(root.join("extra.rs"), "unreviewed").unwrap();
        assert!(verify(&root, &expected).is_err());
        fs::remove_file(root.join("a.rs")).unwrap();
        fs::remove_file(root.join("extra.rs")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
