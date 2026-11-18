//! Read the admitted runtime binding; never infer it from PATH or a package's
//! self-reported identity. The commercial owner verifies this same descriptor.
use super::package::{digest_bytes, file_hash, id, regular, small, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

const FILES: [&str; 8] = [
    "susi-commercial.exe",
    "susi-desktop.exe",
    "susi-plug-host.exe",
    "susi-ui.exe",
    "release-manifest.json",
    "LICENSES/Apache-2.0.txt",
    "THIRD_PARTY_NOTICES",
    "THIRD_PARTY_LICENSES.txt",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    format: String,
    test_release: bool,
    core_release: String,
    files: BTreeMap<String, String>,
}

pub(crate) struct PairedRuntime {
    pub(crate) path: PathBuf,
    // Deny replacement/deletion of the verified Windows image through launch.
    _pin: File,
}

fn resolve(local: &Path, channel: &str, core: &str) -> Result<PairedRuntime> {
    if !local.is_absolute() || !id(core) {
        return Err("invalid installed runtime root or selected core release".into());
    }
    let test = match channel {
        "desktop" => false,
        "desktop-test" => true,
        _ => return Err("invalid installed desktop channel".into()),
    };
    let channel = local.join("SusiLocal").join(channel);
    let trusted = String::from_utf8(small(&channel.join("pairs").join(core))?)?;
    if !id(&trusted) {
        return Err("invalid installed desktop release ID".into());
    }
    let slot = channel.join("slots").join(&trusted);
    let bytes = small(&slot.join("desktop-delivery-v1.json"))?;
    if digest_bytes(&bytes) != trusted {
        return Err("desktop delivery descriptor differs from its trusted release ID".into());
    }
    let delivery: Delivery = serde_json::from_slice(&bytes)?;
    if delivery.format != "susi-desktop-delivery-v1"
        || delivery.core_release != core
        || delivery.test_release != test
        || delivery.files.len() != FILES.len()
        || !FILES
            .iter()
            .all(|name| delivery.files.get(*name).is_some_and(|value| id(value)))
    {
        return Err("desktop binding does not match the selected core and channel".into());
    }
    let path = slot.join("susi-commercial.exe");
    let mut pin = regular(&path)?;
    if delivery.files.get("susi-commercial.exe") != Some(&file_hash(&mut pin)?) {
        return Err("installed commercial runtime differs from its admitted digest".into());
    }
    Ok(PairedRuntime {
        path: path.canonicalize()?,
        _pin: pin,
    })
}

pub(crate) fn paired_runtime() -> Result<Option<PairedRuntime>> {
    let Some(core) = std::env::var_os("SUSI_INSTALLED_RELEASE") else {
        return Ok(None);
    };
    let core = core.to_str().ok_or("invalid installed core release ID")?;
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or("Windows local application directory is unavailable")?;
    let channel =
        std::env::var_os("SUSI_INSTALLED_DESKTOP_CHANNEL").unwrap_or_else(|| "desktop".into());
    let channel = channel
        .to_str()
        .ok_or("invalid installed desktop channel")?;
    resolve(Path::new(&local), channel, core).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn admitted_runtime_requires_exact_pair_channel_descriptor_and_bytes() {
        let temp = susi_paths::tempfile::tempdir().unwrap();
        let root = temp.path();
        let core = "a".repeat(64);
        let runtime = b"admitted runtime bytes";
        let files: BTreeMap<_, _> = FILES
            .iter()
            .map(|name| (*name, digest_bytes(runtime)))
            .collect();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format":"susi-desktop-delivery-v1", "test_release":false,
            "core_release":core, "files":files
        }))
        .unwrap();
        let trusted = digest_bytes(&bytes);
        let channel = root.join("SusiLocal/desktop");
        let slot = channel.join("slots").join(&trusted);
        fs::create_dir_all(&slot).unwrap();
        fs::create_dir_all(channel.join("pairs")).unwrap();
        fs::write(channel.join("pairs").join(&core), &trusted).unwrap();
        fs::write(slot.join("desktop-delivery-v1.json"), &bytes).unwrap();
        fs::write(slot.join("susi-commercial.exe"), runtime).unwrap();
        assert_eq!(
            resolve(root, "desktop", &core).unwrap().path,
            slot.join("susi-commercial.exe").canonicalize().unwrap()
        );
        assert!(resolve(root, "other", &core).is_err());
        assert!(resolve(root, "desktop-test", &core).is_err());
        assert!(resolve(root, "desktop", &"b".repeat(64)).is_err());
        fs::write(slot.join("susi-commercial.exe"), b"replaced runtime").unwrap();
        assert!(resolve(root, "desktop", &core).is_err());
        fs::write(slot.join("susi-commercial.exe"), runtime).unwrap();
        fs::write(slot.join("desktop-delivery-v1.json"), b"{}").unwrap();
        assert!(resolve(root, "desktop", &core).is_err());
    }
}
