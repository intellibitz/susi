use super::*;
use std::collections::BTreeSet;
use std::path::Path;

pub(in crate::distill_compose) fn adjacent_helper_path(
    executable: &Path,
    inherited: Option<std::ffi::OsString>,
) -> Result<Option<std::ffi::OsString>> {
    let directory = executable
        .parent()
        .ok_or_else(|| "distiller executable has no directory".to_owned())?;
    let helper = directory.join(if cfg!(windows) {
        "push-policy.exe"
    } else {
        "push-policy"
    });
    match helper.try_exists() {
        Ok(false) => return Ok(None),
        Ok(true) if helper.is_file() => {}
        Ok(true) => return Err("adjacent push-policy is not a file".to_owned()),
        Err(error) => return Err(format!("cannot inspect adjacent push-policy: {error}")),
    }
    let mut paths = vec![directory.to_path_buf()];
    if let Some(value) = inherited {
        paths.extend(std::env::split_paths(&value));
    }
    std::env::join_paths(paths)
        .map(Some)
        .map_err(|e| format!("cannot prepare native hook search path: {e}"))
}

pub(in crate::distill_compose) fn normalize_endpoint(root: &Path, value: &str) -> Result<String> {
    if value.chars().any(char::is_control) || value.starts_with('-') {
        return Err("invalid Git endpoint".to_owned());
    }
    if let Some((scheme, remainder)) = value.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return Err(
                "unsupported endpoint scheme; use a Git URL or an absolute filesystem path"
                    .to_owned(),
            );
        }
        let authority = remainder.split('/').next().unwrap_or("");
        if authority.is_empty()
            || authority
                .split_once('@')
                .is_some_and(|(user, _)| scheme != "ssh" || user.contains(':'))
        {
            return Err(
                "embedded endpoint credentials are not supported; use Git's credential or SSH helper"
                    .to_owned(),
            );
        }
        return Ok(value.to_owned());
    }
    if !Path::new(value).is_absolute() && value.contains(':') {
        let (host, path) = value
            .split_once(':')
            .ok_or_else(|| "invalid SSH endpoint".to_owned())?;
        if host.is_empty() || path.is_empty() || path.starts_with(':') || host.contains(['/', '\\'])
        {
            return Err("unsupported Git remote helper endpoint".to_owned());
        }
        return Ok(value.to_owned());
    }
    let path = root
        .join(value)
        .canonicalize()
        .map_err(|e| format!("cannot resolve filesystem endpoint: {e}"))?;
    let value = path
        .to_str()
        .ok_or_else(|| "Git endpoint is not UTF-8".to_owned())?;
    Ok(if let Some(unc) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        value.strip_prefix("\\\\?\\").unwrap_or(value).to_owned()
    })
}

pub(in crate::distill_compose) fn parse_cli(args: &[String]) -> Result<ParsedCli> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return if args.is_empty() {
            Err(USAGE.to_owned())
        } else {
            Ok(ParsedCli::Help)
        };
    }
    if args[0] == "submit-main" {
        return parse_submit_main_flags(&args[1..]);
    }
    if args[0] == "verify-harvest" {
        return parse_verify_harvest_flags(&args[1..]);
    }
    if args[0] == "quarantine-fetch" {
        return parse_quarantine_fetch_flags(&args[1..]);
    }
    if args[0] == "release-record" {
        return parse_release_record_flags(&args[1..]);
    }
    if args[0] == "publish-branch" {
        return Err(RELEASE_BRANCH_STORAGE_ONLY.to_owned());
    }
    let mode = match args[0].as_str() {
        "plan" => CommandMode::Plan,
        "compose" => CommandMode::Compose,
        "orchestrate" => CommandMode::Orchestrate,
        "publish" => CommandMode::Publish,
        "status" => CommandMode::Status,
        _ => return Err(USAGE.to_owned()),
    };
    if matches!(mode, CommandMode::Publish | CommandMode::Status) {
        if args.len() < 2 || args.len() > 3 {
            return Err(USAGE.to_owned());
        }
        let json_output = if args.len() == 3 {
            if args[2] != "--json" {
                return Err(USAGE.to_owned());
            }
            true
        } else {
            false
        };
        valid_name(&args[1])?;
        return Ok(ParsedCli::Inspect(InspectArgs {
            mode,
            name: args[1].clone(),
            json_output,
        }));
    }
    parse_distill_flags(mode, &args[1..])
}

pub(in crate::distill_compose) fn parse_submit_main_flags(rest: &[String]) -> Result<ParsedCli> {
    let mut rev: Option<String> = None;
    let mut max_attempts: usize = 4;
    let mut json_output = false;
    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--rev" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if rev.replace(val.clone()).is_some() {
                    return Err("duplicate --rev".to_owned());
                }
            }
            "--max-attempts" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                let parsed: usize = val
                    .parse()
                    .map_err(|_| "invalid --max-attempts integer".to_owned())?;
                if parsed == 0 || parsed > 16 {
                    return Err("--max-attempts must be between 1 and 16".to_owned());
                }
                max_attempts = parsed;
            }
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }
    Ok(ParsedCli::SubmitMain(SubmitMainArgs {
        rev,
        max_attempts,
        json_output,
    }))
}

pub(in crate::distill_compose) fn parse_verify_harvest_flags(rest: &[String]) -> Result<ParsedCli> {
    let mut rev: Option<String> = None;
    let mut range: Option<String> = None;
    let mut strict = false;
    let mut json_output = false;
    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--rev" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if rev.replace(val.clone()).is_some() {
                    return Err("duplicate --rev".to_owned());
                }
            }
            "--range" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if range.replace(val.clone()).is_some() {
                    return Err("duplicate --range".to_owned());
                }
            }
            "--strict" => strict = true,
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }
    Ok(ParsedCli::VerifyHarvest(VerifyHarvestArgs {
        rev,
        range,
        strict,
        json_output,
    }))
}

pub(in crate::distill_compose) fn parse_quarantine_fetch_flags(
    rest: &[String],
) -> Result<ParsedCli> {
    if rest.len() < 4 {
        return Err(USAGE.to_owned());
    }
    let url = rest[0].clone();
    let sha = rest[1].clone();
    let mut slug: Option<String> = None;
    let mut json_output = false;
    let mut idx = 2usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--slug" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if slug.replace(val.clone()).is_some() {
                    return Err("duplicate --slug".to_owned());
                }
            }
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }
    let slug = slug.ok_or_else(|| "missing --slug <slug>".to_owned())?;
    Ok(ParsedCli::QuarantineFetch(QuarantineFetchArgs {
        url,
        sha,
        slug,
        json_output,
    }))
}

pub(in crate::distill_compose) fn valid_semver_triplet(text: &str) -> bool {
    let mut parts = text.split('.');
    let ok_num = |part: Option<&str>| {
        part.filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse::<u64>().ok())
            .is_some()
    };
    ok_num(parts.next()) && ok_num(parts.next()) && ok_num(parts.next()) && parts.next().is_none()
}

pub(in crate::distill_compose) fn parse_release_asset_spec(raw: &str) -> Result<ReleaseAssetSpec> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 5 {
        return Err(
            "invalid --asset; expected <file>:<target>:<package>:<bin>:<sha256>".to_owned(),
        );
    }
    let file = parts[0].trim().to_owned();
    let target = parts[1].trim().to_owned();
    let package = parts[2].trim().to_owned();
    let bin = parts[3].trim().to_owned();
    let sha256 = parts[4].trim().to_ascii_lowercase();
    if !matches!(
        target.as_str(),
        "x86_64-pc-windows-msvc" | "x86_64-unknown-linux-gnu"
    ) {
        return Err(format!("unsupported release target '{target}'"));
    }
    if package.is_empty() || bin.is_empty() {
        return Err("asset package and bin must be non-empty".to_owned());
    }
    let expected_file = if target == "x86_64-pc-windows-msvc" {
        format!("{bin}-windows-x86_64.exe")
    } else {
        format!("{bin}-linux-x86_64")
    };
    if file != expected_file {
        return Err(format!(
            "asset file '{file}' must equal canonical '{expected_file}' for target {target}"
        ));
    }
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("asset sha256 must be 64 lowercase hex characters".to_owned());
    }
    Ok(ReleaseAssetSpec {
        file,
        target,
        package,
        bin,
        sha256,
    })
}

pub(in crate::distill_compose) fn parse_release_record_flags(rest: &[String]) -> Result<ParsedCli> {
    let mut name: Option<String> = None;
    let mut tag: Option<String> = None;
    let mut commit: Option<String> = None;
    let mut tagged = "2026-11-11T00:00:00Z".to_owned();
    let mut toolchain: Option<String> = None;
    let mut host: Option<String> = None;
    let mut assets = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut json_output = false;

    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--name" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if name.replace(val.clone()).is_some() {
                    return Err("duplicate --name".to_owned());
                }
            }
            "--tag" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if tag.replace(val.clone()).is_some() {
                    return Err("duplicate --tag".to_owned());
                }
            }
            "--commit" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if commit.replace(val.clone()).is_some() {
                    return Err("duplicate --commit".to_owned());
                }
            }
            "--tagged" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                tagged = val.clone();
            }
            "--toolchain" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if toolchain.replace(val.clone()).is_some() {
                    return Err("duplicate --toolchain".to_owned());
                }
            }
            "--host" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if host.replace(val.clone()).is_some() {
                    return Err("duplicate --host".to_owned());
                }
            }
            "--asset" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                assets.push(parse_release_asset_spec(val)?);
            }
            "--out" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if out.replace(PathBuf::from(val)).is_some() {
                    return Err("duplicate --out".to_owned());
                }
            }
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }
    let name = name.ok_or_else(|| "missing --name <name>".to_owned())?;
    valid_name(&name)?;
    let tag = tag.ok_or_else(|| "missing --tag <vX.Y.Z>".to_owned())?;
    let Some(ver) = tag.strip_prefix('v') else {
        return Err("--tag must start with 'v' (vX.Y.Z)".to_owned());
    };
    if !valid_semver_triplet(ver) {
        return Err(format!("--tag '{tag}' does not carry a valid X.Y.Z semver"));
    }
    Ok(ParsedCli::ReleaseRecord(ReleaseRecordArgs {
        name,
        tag,
        commit,
        tagged,
        toolchain,
        host,
        assets,
        out,
        json_output,
    }))
}

pub(in crate::distill_compose) fn parse_distill_flags(
    mode: CommandMode,
    rest: &[String],
) -> Result<ParsedCli> {
    let mut name: Option<String> = None;
    let mut base_rev: Option<String> = None;
    let mut from_rev: Option<String> = None;
    let mut selected = Vec::new();
    let mut tasks_from_range: Option<String> = None;
    let mut feature_spec: Option<String> = None;
    let mut plan_manifest: Option<PathBuf> = None;
    let mut functional_only = false;
    let mut verify_accept = false;
    let mut publish_snapshot = false;
    let mut publish_release_branch = false;
    let mut release_candidate = false;
    let mut pull = false;
    let mut strict = false;
    let mut adopt_host_commits: BTreeSet<String> = BTreeSet::new();
    let mut receipt_out: Option<PathBuf> = None;
    let mut release_tag: Option<String> = None;
    let mut release_record_out: Option<PathBuf> = None;
    let mut json_output = false;

    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--name" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if name.replace(val.clone()).is_some() {
                    return Err("duplicate --name".to_owned());
                }
            }
            "--base" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if base_rev.replace(val.clone()).is_some() {
                    return Err("duplicate --base".to_owned());
                }
            }
            "--from" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if from_rev.replace(val.clone()).is_some() {
                    return Err("duplicate --from".to_owned());
                }
            }
            "--select" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                for item in val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    selected.push(item.to_owned());
                }
            }
            "--tasks-from-range" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if tasks_from_range.replace(val.clone()).is_some() {
                    return Err("duplicate --tasks-from-range".to_owned());
                }
            }
            "--feature" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if feature_spec.replace(val.clone()).is_some() {
                    return Err("duplicate --feature".to_owned());
                }
            }
            "--plan-manifest" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if plan_manifest.replace(PathBuf::from(val)).is_some() {
                    return Err("duplicate --plan-manifest".to_owned());
                }
            }
            "--receipt-out" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if receipt_out.replace(PathBuf::from(val)).is_some() {
                    return Err("duplicate --receipt-out".to_owned());
                }
            }
            "--release-tag" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                let Some(ver) = val.strip_prefix('v') else {
                    return Err("--release-tag must start with 'v' (vX.Y.Z)".to_owned());
                };
                if !valid_semver_triplet(ver) {
                    return Err(format!(
                        "--release-tag '{val}' does not carry a valid X.Y.Z semver"
                    ));
                }
                if release_tag.replace(val.clone()).is_some() {
                    return Err("duplicate --release-tag".to_owned());
                }
            }
            "--release-record-out" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if release_record_out.replace(PathBuf::from(val)).is_some() {
                    return Err("duplicate --release-record-out".to_owned());
                }
            }
            "--functional-only" => functional_only = true,
            "--verify-accept" => verify_accept = true,
            "--publish-snapshot" => publish_snapshot = true,
            "--publish-release-branch" => publish_release_branch = true,
            "--release-candidate" => release_candidate = true,
            "--pull" => pull = true,
            "--strict" => strict = true,
            "--adopt-host-commit" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                adopt_host_commits.extend(hostmerge::parse_adoptions(std::slice::from_ref(val))?);
            }
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }

    if verify_accept && !matches!(mode, CommandMode::Compose | CommandMode::Orchestrate) {
        return Err("--verify-accept requires compose or orchestrate mode".to_owned());
    }
    if publish_snapshot && mode != CommandMode::Orchestrate {
        return Err("--publish-snapshot requires orchestrate mode".to_owned());
    }
    if publish_release_branch {
        return Err(RELEASE_BRANCH_STORAGE_ONLY.to_owned());
    }
    let name = name.ok_or_else(|| "missing --name <name>".to_owned())?;
    valid_name(&name)?;
    let base_rev = base_rev.ok_or_else(|| "missing --base <rev>".to_owned())?;
    if selected.is_empty()
        && tasks_from_range.is_none()
        && feature_spec.is_none()
        && plan_manifest.is_none()
        && mode != CommandMode::Orchestrate
    {
        return Err(
            "specify at least one --select <TASK_ID>, --tasks-from-range <A..B>, --feature <manifest>, or --plan-manifest <path>"
                .to_owned(),
        );
    }
    if release_record_out.is_some() && release_tag.is_none() {
        return Err("--release-record-out requires --release-tag <vX.Y.Z>".to_owned());
    }
    Ok(ParsedCli::Distill(DistillArgs {
        mode,
        name,
        base_rev,
        from_rev,
        selected,
        tasks_from_range,
        feature_spec,
        plan_manifest,
        functional_only,
        verify_accept,
        publish_snapshot,
        release_candidate,
        pull,
        strict,
        adopt_host_commits,
        receipt_out,
        release_tag,
        release_record_out,
        json_output,
    }))
}
