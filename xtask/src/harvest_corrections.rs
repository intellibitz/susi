//! Explicit repairs of immutable historical Harvest labels, bound to Git evidence.
use super::{
    fs, harvest_trailers, list_tree, normalize_rel, BTreeMap, BTreeSet, Git, Manifest, Result,
    Value,
};

const PATH: &str = ".agents/harvest-corrections.json";
const SCHEMA: &str = "susi-harvest-trailer-corrections-v1";

fn reject_duplicate_fields(raw: &[u8]) -> Result<()> {
    // JSON has already parsed successfully. Inspect object keys before Value's
    // last-value behavior can hide conflicting evidence, including escaped keys.
    let mut objects: Vec<BTreeSet<String>> = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        match raw[at] {
            b'{' => objects.push(BTreeSet::new()),
            b'}' => {
                objects.pop();
            }
            b'"' => {
                let start = at;
                at += 1;
                while at < raw.len() && raw[at] != b'"' {
                    if raw[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                let mut next = at + 1;
                while next < raw.len() && raw[next].is_ascii_whitespace() {
                    next += 1;
                }
                if raw.get(next) == Some(&b':') {
                    let key: String = serde_json::from_slice(&raw[start..=at])
                        .map_err(|error| error.to_string())?;
                    if !objects
                        .last_mut()
                        .ok_or("correction key outside object")?
                        .insert(key)
                    {
                        return Err("duplicate harvest correction JSON field".into());
                    }
                }
            }
            _ => {}
        }
        at += 1;
    }
    Ok(())
}

fn object<'a>(value: &'a Value, fields: &[&str]) -> Result<&'a serde_json::Map<String, Value>> {
    let object = value.as_object().ok_or("correction must be an object")?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err("correction has missing or unbound fields".into());
    }
    Ok(object)
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("correction requires {field}"))
}

fn full_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn generated_metadata(path: &str) -> bool {
    path == super::NOTICES_FILE
        || path == super::APACHE_TEXT_PATH
        || [".agents/harvest/", ".agents/tasks/"].iter().any(|prefix| {
            path.strip_prefix(prefix)
                .is_some_and(|name| !name.contains('/') && name.ends_with(".json"))
        })
}

fn verify_files(git: &Git, commit: &str, row: &Value, manifest: &Manifest) -> Result<()> {
    // A correction covers an ordinary import commit, not an ambiguous merge.
    let parents = git.text(&["rev-list", "--parents", "-n", "1", commit])?;
    if parents.split_whitespace().count() != 2 {
        return Err("correction requires a commit with exactly one parent".into());
    }
    let raw = git.checked(
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "--diff-filter=A",
            "-r",
            "-z",
            commit,
            "--",
        ],
        None,
    )?;
    let added: BTreeSet<String> = raw
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_owned)
                .map_err(|_| "correction source path is not UTF-8".to_owned())
        })
        .collect::<Result<_>>()?;
    let added: BTreeSet<_> = added
        .into_iter()
        .filter(|path| !generated_metadata(path))
        .collect();
    if added.is_empty() {
        return Err("correction has no imported source files".into());
    }
    let files = row["files"]
        .as_array()
        .filter(|files| !files.is_empty() && files.len() <= 4096)
        .ok_or("correction requires a bounded nonempty source-file list")?;
    let tree = list_tree(git, commit)?;
    let mut covered = BTreeSet::new();
    for file in files {
        object(file, &["path", "commit_blob_sha1"])?;
        let path = text(file, "path")?;
        if normalize_rel(path, true).as_deref() != Ok(path) || !covered.insert(path.to_owned()) {
            return Err("correction source path is invalid or duplicated".into());
        }
        let blob = text(file, "commit_blob_sha1")?;
        if !full_oid(blob)
            || !tree.get(path).is_some_and(|entry| {
                entry.kind == "blob"
                    && matches!(entry.mode.as_str(), "100644" | "100755")
                    && entry.oid == blob
            })
        {
            return Err("correction source blob differs from the immutable commit".into());
        }
        if !manifest.files.iter().any(|source| source.dest_path == path) {
            return Err(format!(
                "replacement manifest does not cover imported source {path}"
            ));
        }
    }
    if covered != added {
        return Err("correction must cover exactly all imported source paths".into());
    }
    Ok(())
}

fn load(git: &Git, manifests: &[Manifest]) -> Result<BTreeMap<(String, String), String>> {
    let path = git.root.join(PATH);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 1024 * 1024
        || !path
            .canonicalize()
            .map_err(|error| error.to_string())?
            .starts_with(git.root.canonicalize().map_err(|error| error.to_string())?)
    {
        return Err("correction record is redirected, nonregular or oversized".into());
    }
    let raw = fs::read(&path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("invalid harvest correction record: {error}"))?;
    reject_duplicate_fields(&raw)?;
    object(&value, &["schema", "corrections"])?;
    if value["schema"] != SCHEMA {
        return Err("unsupported harvest correction schema".into());
    }
    let rows = value["corrections"]
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 128)
        .ok_or("correction record requires a bounded nonempty list")?;
    let mut corrections = BTreeMap::new();
    for row in rows {
        object(
            row,
            &["commit", "old_label", "manifest_id", "files", "reason"],
        )?;
        let commit = text(row, "commit")?;
        let old = text(row, "old_label")?;
        let replacement = text(row, "manifest_id")?;
        let reason = text(row, "reason")?;
        if !full_oid(commit)
            || old
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ',')
            || reason.trim().is_empty()
        {
            return Err("invalid correction commit, exact label or reason".into());
        }
        let manifest = manifests
            .iter()
            .find(|manifest| manifest.id == replacement)
            .ok_or("correction replacement manifest does not exist or is invalid")?;
        if manifests.iter().any(|manifest| manifest.id == old) {
            return Err("correction cannot replace an existing valid manifest label".into());
        }
        if !git.has_commit(commit)?
            || !git
                .output(&["merge-base", "--is-ancestor", commit, "HEAD"], None)?
                .status
                .success()
        {
            return Err("correction commit is not in this checkout's history".into());
        }
        let trailers = harvest_trailers(git, &format!("{commit}^!"))?;
        if trailers.len() != 1
            || trailers[0].0 != commit
            || trailers[0]
                .1
                .iter()
                .filter(|label| label.as_str() == old)
                .count()
                != 1
        {
            return Err(
                "correction does not match the immutable commit's exact Harvest label".into(),
            );
        }
        verify_files(git, commit, row, manifest)?;
        if corrections
            .insert((commit.to_owned(), old.to_owned()), replacement.to_owned())
            .is_some()
        {
            return Err("duplicate harvest correction".into());
        }
    }
    Ok(corrections)
}

pub(super) fn audit_history(
    git: &Git,
    manifests: &[Manifest],
    range: Option<String>,
    violations: &mut Vec<String>,
) -> Result<usize> {
    let corrections = load(git, manifests)?;
    let manifest_dir = super::MANIFEST_DIR;
    let mut trailer_commits = 0;
    if let Some(range) = range {
        let known: BTreeSet<String> = super::load_manifest_values(&git.root)?
            .into_iter()
            .map(|(name, _)| name.trim_end_matches(".json").to_owned())
            .collect();
        for (commit, ids) in harvest_trailers(git, &range)? {
            trailer_commits += 1;
            for id in ids.iter().filter(|id| !known.contains(*id)) {
                if corrections.contains_key(&(commit.clone(), id.clone())) {
                    continue;
                }
                violations.push(format!(
                    "commit {} has `Harvest: {id}` but {manifest_dir}/{id}.json does not exist",
                    &commit[..commit.len().min(12)]
                ));
            }
        }
    }
    Ok(trailer_commits)
}
