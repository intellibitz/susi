//! Portable verification of a producer-admitted release. The release ID is received
//! from the trusted qualifier, never inferred from the package's own manifest.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(super) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub(super) const BIN_NAME: &str = if cfg!(windows) { "susi.exe" } else { "susi" };
pub(super) const SIDECARS: [(&str, &str); 3] = [
    ("sbom.json", "sbom-sha256"),
    ("THIRD-PARTY-NOTICES.txt", "notices-sha256"),
    ("native-source-audit.txt", "native-audit-sha256"),
];
pub(super) const QUALIFICATION_POLICY: &[u8] = include!("qualification_policy.rs").as_bytes();

pub(super) fn expected_host_target_triple() -> String {
    if cfg!(target_os = "windows") {
        format!("{}-pc-windows-msvc", std::env::consts::ARCH)
    } else if cfg!(target_os = "macos") {
        format!("{}-apple-darwin", std::env::consts::ARCH)
    } else {
        format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
    }
}

pub(super) fn is_reparse_or_symlink(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        meta.file_type().is_symlink() || (meta.file_attributes() & 0x400) != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

pub(super) fn id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn digest_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(super) fn file_hash(file: &mut File) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex(&hash.finalize()))
}

pub(super) fn regular(path: &Path) -> Result<File> {
    for parent in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || is_reparse_or_symlink(&meta) {
            return Err(format!("redirected release path: {}", parent.display()).into());
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || is_reparse_or_symlink(&meta) {
        return Err(format!("not an ordinary release file: {}", path.display()).into());
    }
    // FILE_SHARE_READ pins these immutable bytes through hashing and process creation.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    options.share_mode(1);
    Ok(options.open(path)?)
}

pub(super) fn small(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    regular(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("release metadata exceeds 64 KiB".into());
    }
    Ok(bytes)
}

pub(super) fn directory(path: &Path) -> Result<()> {
    super::super::directory(path)
}

pub(super) fn link_or_copy(source: &Path, destination: &Path) -> Result<()> {
    if fs::hard_link(source, destination).is_ok() {
        return Ok(());
    }
    let mut input = regular(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(source) {
            if (meta.permissions().mode() & 0o111) != 0 {
                fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
            }
        }
    }
    Ok(())
}

pub(super) fn atomic_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("susi-pending");
    match fs::symlink_metadata(&temporary) {
        Ok(_) => {
            drop(regular(&temporary)?);
            fs::remove_file(&temporary)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    drop(out);
    fs::rename(temporary, path)?;
    Ok(())
}

pub(super) struct Package {
    pub(super) root: PathBuf,
    pub(super) release: String,
    artifact: String,
    bytes: u64,
    gates: String,
    sidecars: Vec<(&'static str, String)>,
}

impl Package {
    pub(super) fn open(root: &Path, release: &str) -> Result<Self> {
        if !id(release) {
            return Err("a trusted lowercase release SHA-256 is required".into());
        }
        let manifest = small(&root.join("manifest.txt"))?;
        if digest_bytes(&manifest) != release {
            return Err("release manifest SHA-256 mismatch".into());
        }
        let mut lines = std::str::from_utf8(&manifest)?.lines();
        if lines.next() != Some("susi-local-release-v1") {
            return Err("unsupported release manifest".into());
        }
        let mut fields = BTreeMap::new();
        for line in lines {
            let (name, value) = line.split_once('=').ok_or("invalid release field")?;
            if fields.insert(name, value).is_some() {
                return Err("duplicate release field".into());
            }
        }
        let get = |name| fields.get(name).copied().ok_or("missing release field");
        if get("host")? != expected_host_target_triple() {
            return Err("release targets another platform".into());
        }
        for field in ["source", "tree", "integration-receipt"] {
            let oid = get(field)?;
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err("invalid release source or integration identity".into());
            }
        }
        for field in [
            "artifact-sha256",
            "lock-sha256",
            "compiler-sha256",
            "gates-sha256",
            "sbom-sha256",
            "notices-sha256",
            "native-audit-sha256",
        ] {
            if !id(get(field)?) {
                return Err(format!("invalid release digest: {field}").into());
            }
        }
        let bytes = get("artifact-bytes")?.parse()?;
        if !(1..=256 * 1024 * 1024).contains(&bytes) {
            return Err("release executable exceeds admitted size".into());
        }
        let mut sidecars = Vec::new();
        for (name, field) in SIDECARS {
            sidecars.push((name, get(field)?.to_owned()));
        }
        Ok(Self {
            root: root.into(),
            release: release.into(),
            artifact: get("artifact-sha256")?.into(),
            bytes,
            gates: get("gates-sha256")?.into(),
            sidecars,
        })
    }

    pub(super) fn executable(&self) -> PathBuf {
        self.root.join(BIN_NAME)
    }

    pub(super) fn pinned_executable(&self) -> Result<File> {
        let mut file = regular(&self.executable())?;
        if file.metadata()?.len() != self.bytes || file_hash(&mut file)? != self.artifact {
            return Err("installed executable differs from its admitted manifest".into());
        }
        Ok(file)
    }

    pub(super) fn verify_sidecars(&self) -> Result<()> {
        for (name, hash) in &self.sidecars {
            if file_hash(&mut regular(&self.root.join(name))?)? != *hash {
                return Err(format!("admitted sidecar changed: {name}").into());
            }
        }
        Ok(())
    }

    pub(super) fn stage(&self, root: &Path) -> Result<Self> {
        let source = self.pinned_executable()?;
        self.verify_sidecars()?;
        let gates = self.root.join("gates.txt");
        let mut header = vec![0u8; QUALIFICATION_POLICY.len()];
        regular(&gates)?.read_exact(&mut header)?;
        if header != QUALIFICATION_POLICY || file_hash(&mut regular(&gates)?)? != self.gates {
            return Err("qualification gates differ from the trusted release manifest".into());
        }
        let destination = root.join("releases").join(&self.release);
        directory(&destination)?;
        let mut names = vec![BIN_NAME, "manifest.txt"];
        names.extend(self.sidecars.iter().map(|(name, _)| *name));
        for name in names {
            let to = destination.join(name);
            match fs::symlink_metadata(&to) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let temp = to.with_extension("susi-pending");
            if temp.try_exists()? {
                drop(regular(&temp)?);
                fs::remove_file(&temp)?;
            }
            if name == BIN_NAME {
                let legacy = root.join("builds").join(&self.artifact).join(BIN_NAME);
                if legacy.try_exists()? {
                    self.verify_entry(&legacy)?;
                    link_or_copy(&legacy, &temp)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
                    }
                    fs::rename(temp, to)?;
                    continue;
                }
            }
            let mut out = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            let mut input = regular(&self.root.join(name))?;
            std::io::copy(&mut input, &mut out)?;
            out.sync_all()?;
            drop(out);
            #[cfg(unix)]
            if name == BIN_NAME {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
            }
            fs::rename(temp, to)?;
        }
        drop(source);
        let installed = Self::open(&destination, &self.release)?;
        installed.verify_sidecars()?;
        installed.pinned_executable()?;
        Ok(installed)
    }

    pub(super) fn verify_entry(&self, entry: &Path) -> Result<()> {
        if file_hash(&mut regular(entry)?)? != self.artifact {
            return Err("installed dispatcher digest mismatch".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_host_target_triple_matches_current_platform() {
        let triple = expected_host_target_triple();
        assert!(triple.starts_with(std::env::consts::ARCH));
        if cfg!(target_os = "windows") {
            assert!(triple.ends_with("-pc-windows-msvc"));
            assert_eq!(BIN_NAME, "susi.exe");
        } else if cfg!(target_os = "macos") {
            assert!(triple.ends_with("-apple-darwin"));
            assert_eq!(BIN_NAME, "susi");
        } else {
            assert!(triple.ends_with("-unknown-linux-gnu"));
            assert_eq!(BIN_NAME, "susi");
        }
    }
}
