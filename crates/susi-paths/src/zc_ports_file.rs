//! Stable discovery file (`ports.json`) for every client.
//!
//! The daemon writes it when it binds the host contract: the five contract
//! ports, the uniform `offset` they sit at, the daemon's `pid`, and
//! `fallback_from` (the offset it was configured for; it differs from
//! `offset` exactly when the daemon fell back to a free block). It is
//! removed when the daemon stops, and a copy whose pid is not the live
//! substrate-lock holder is never trusted ([`crate::ports::live_ports_in`]).

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// File name of the discovery document inside the config dir.
pub const FILE_NAME: &str = "ports.json";

/// Snapshot published for clients that must not hardcode 9090–9094.
#[derive(Debug, Clone, PartialEq)]
pub struct PortsFile {
    pub path: PathBuf,
    pub ports: BTreeMap<String, u16>,
    pub token_path: PathBuf,
    pub api_token_path: PathBuf,
    pub tls_enabled: bool,
    /// Uniform offset the contract ports sit at.
    pub offset: u16,
    /// Pid of the daemon that wrote the file; 0 when unknown (a legacy file).
    pub pid: u32,
    /// The offset the daemon was configured for (0 unless pinned).
    pub fallback_from: u16,
}

impl PortsFile {
    /// Build the discovery document for `home` (typically `~/.susi`), with no
    /// daemon identity: `pid` 0, `fallback_from` equal to `offset`.
    #[must_use]
    pub fn for_home(home: PathBuf, offset: u16, tls_enabled: bool) -> Self {
        let mut ports = BTreeMap::new();
        let named = [
            ("gmcp", crate::ports::GMCP),
            ("gemi", crate::ports::GEMI),
            ("udp_discovery", crate::ports::UDP_DISCOVERY),
            ("gmcp_http", crate::ports::GMCP_HTTP),
            ("a2a_http", crate::ports::A2A_HTTP),
        ];
        for (key, base) in named {
            ports.insert(key.to_string(), base.saturating_add(offset));
        }
        Self {
            path: home.join(FILE_NAME),
            ports,
            token_path: home.join("token"),
            api_token_path: home.join("api_token"),
            tls_enabled,
            offset,
            pid: 0,
            fallback_from: offset,
        }
    }

    /// The document the daemon `pid` publishes for the block it bound at
    /// `offset`, having been configured for `fallback_from`.
    #[must_use]
    pub fn for_daemon(
        home: PathBuf,
        offset: u16,
        fallback_from: u16,
        pid: u32,
        tls_enabled: bool,
    ) -> Self {
        Self {
            pid,
            fallback_from,
            ..Self::for_home(home, offset, tls_enabled)
        }
    }

    /// Whether the daemon serves a fallback block, not the configured one.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        self.offset != self.fallback_from
    }

    /// Serialize to the JSON clients read.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ports": self.ports,
            "token_path": self.token_path.to_string_lossy(),
            "api_token_path": self.api_token_path.to_string_lossy(),
            "tls": self.tls_enabled,
            "offset": self.offset,
            "pid": self.pid,
            "fallback_from": self.fallback_from,
            "fallback": self.is_fallback(),
        })
    }

    /// Atomically write `ports.json` under its parent directory via a sibling
    /// staging file (`ports.json.tmp.<pid>`).
    pub fn write_to_home(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(&self.to_json()).map_err(std::io::Error::other)?;
        let tmp_name = format!(
            "{}.tmp.{}",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(FILE_NAME),
            std::process::id()
        );
        let tmp_path = self.path.with_file_name(tmp_name);
        std::fs::write(&tmp_path, body.as_bytes())?;
        if let Err(err) = std::fs::rename(&tmp_path, &self.path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }

    /// Read `home/ports.json` and reconstruct [`PortsFile`].
    ///
    /// A file written before `offset`, `pid` and `fallback_from` existed
    /// still parses: its offset is read back from the GMCP port, its pid is
    /// 0 (so it never counts as a live daemon's), and it is no fallback.
    #[must_use]
    pub fn read_from_home(home: &Path) -> Option<Self> {
        let path = home.join(FILE_NAME);
        let raw = std::fs::read_to_string(&path).ok()?;
        let val: Value = serde_json::from_str(&raw).ok()?;
        let ports_obj = val.get("ports")?.as_object()?;
        let mut ports = BTreeMap::new();
        for (k, v) in ports_obj {
            let port = u16::try_from(v.as_u64()?).ok()?;
            ports.insert(k.clone(), port);
        }
        let token_path = val
            .get("token_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("token"));
        let api_token_path = val
            .get("api_token_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("api_token"));
        let tls_enabled = val.get("tls").and_then(Value::as_bool).unwrap_or(false);
        let field_u16 = |key: &str| {
            val.get(key)
                .and_then(Value::as_u64)
                .and_then(|v| u16::try_from(v).ok())
        };
        let offset = match val.get("offset") {
            Some(_) => field_u16("offset")?,
            None => ports
                .get("gmcp")
                .map_or(0, |gmcp| gmcp.saturating_sub(crate::ports::GMCP)),
        };
        let pid = val
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let fallback_from = field_u16("fallback_from").unwrap_or(offset);
        Some(Self {
            path,
            ports,
            token_path,
            api_token_path,
            tls_enabled,
            offset,
            pid,
            fallback_from,
        })
    }

    /// Remove `home/ports.json` when it names `pid` (the daemon's own clean
    /// stop). Returns whether a file was removed.
    pub fn remove_if_written_by(home: &Path, pid: u32) -> bool {
        Self::read_from_home(home).is_some_and(|pf| pf.pid == pid)
            && std::fs::remove_file(home.join(FILE_NAME)).is_ok()
    }

    /// Remove `home/ports.json` unless the live substrate-lock holder wrote
    /// it: after a stop, a crash or a refused start nothing may keep
    /// claiming a block. Returns whether a file was removed.
    pub fn remove_unless_live(home: &Path) -> bool {
        let path = home.join(FILE_NAME);
        if !path.exists() {
            return false;
        }
        let live = Self::read_from_home(home)
            .is_some_and(|pf| crate::ports::daemon_lock_live(home, pf.pid));
        !live && std::fs::remove_file(path).is_ok()
    }
}
#[cfg(test)]
mod zc_ports_file_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn zc_ports_file_publishes_offset_ports_not_hardcoded() {
        let pf = PortsFile::for_home(PathBuf::from("/tmp/susi-home"), 100, true);
        assert_eq!(pf.path, PathBuf::from("/tmp/susi-home/ports.json"));
        assert_eq!(pf.ports.get("gmcp"), Some(&9190));
        assert_eq!(pf.ports.get("gemi"), Some(&9191));
        assert_eq!(pf.api_token_path, PathBuf::from("/tmp/susi-home/api_token"));
        let v = pf.to_json();
        assert_eq!(v["tls"], true);
        assert!(v["token_path"].as_str().unwrap().ends_with("token"));
        assert!(v["api_token_path"].as_str().unwrap().ends_with("api_token"));
    }

    #[test]
    fn zc_ports_file_write_and_read_roundtrip_preserves_discovery_contract() {
        let tmp = std::env::temp_dir().join(format!(
            "susi-zc-ports-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let pf = PortsFile::for_home(tmp.clone(), 250, false);
        pf.write_to_home().expect("write_to_home must succeed");
        assert!(pf.path.exists(), "ports.json must exist on disk");

        let loaded = PortsFile::read_from_home(&tmp).expect("read_from_home must parse ports.json");
        assert_eq!(loaded, pf);
        assert_eq!(loaded.ports.get("gmcp_http"), Some(&9343));
        assert_eq!(loaded.ports.get("a2a_http"), Some(&9344));

        // Verify fallback when reading legacy ports.json without api_token_path.
        std::fs::write(
            &pf.path,
            serde_json::to_string(&json!({
                "ports": { "gmcp": 9090 },
                "token_path": tmp.join("token").to_string_lossy(),
                "tls": true
            }))
            .unwrap(),
        )
        .unwrap();
        let legacy = PortsFile::read_from_home(&tmp).expect("legacy ports.json must parse");
        assert_eq!(legacy.api_token_path, tmp.join("api_token"));
        assert!(legacy.tls_enabled);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-ports-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn for_home_saturates_every_port_at_u16_max() {
        let pf = PortsFile::for_home(PathBuf::from("home"), u16::MAX, false);
        assert_eq!(pf.ports.len(), 5);
        assert!(pf.ports.values().all(|p| *p == u16::MAX), "{:?}", pf.ports);
        assert!(!pf.tls_enabled);
    }

    #[test]
    fn read_from_home_rejects_missing_or_malformed_documents() {
        assert_eq!(
            PortsFile::read_from_home(&std::env::temp_dir().join("susi-ports-no-such-dir-xyz")),
            None
        );
        let home = scratch("reject");
        for body in [
            "{not json",
            "{}",
            r#"{"ports": [9090]}"#,
            r#"{"ports": {"gmcp": 70000}}"#,
            r#"{"ports": {"gmcp": "9090"}}"#,
            r#"{"ports": {"gmcp": -1}}"#,
        ] {
            std::fs::write(home.join("ports.json"), body).unwrap();
            assert_eq!(PortsFile::read_from_home(&home), None, "{body}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn read_from_home_defaults_token_paths_and_tls_when_omitted() {
        let home = scratch("defaults");
        std::fs::write(home.join("ports.json"), r#"{"ports": {"gmcp": 9090}}"#).unwrap();
        let pf = PortsFile::read_from_home(&home).unwrap();
        assert_eq!(pf.path, home.join("ports.json"));
        assert_eq!(pf.ports.get("gmcp"), Some(&9090));
        assert_eq!(pf.token_path, home.join("token"));
        assert_eq!(pf.api_token_path, home.join("api_token"));
        assert!(!pf.tls_enabled);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_failed_publish_reports_the_error_and_leaves_no_staging_file() {
        let home = scratch("publish-fail");
        // A directory where ports.json belongs makes the final rename fail.
        std::fs::create_dir_all(home.join("ports.json")).unwrap();
        let pf = PortsFile::for_home(home.clone(), 0, false);
        assert!(pf.write_to_home().is_err());
        let leftovers: Vec<_> = std::fs::read_dir(&home)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("ports.json.tmp."))
            .collect();
        assert!(leftovers.is_empty(), "orphan staging files: {leftovers:?}");
        assert!(home.join("ports.json").is_dir());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Make `home` look like the substrate dir of a live daemon with this
    /// test process's pid: the pid record, and on Windows an OS lock on
    /// `substrate.lock` held by the returned handle (the daemon holds its
    /// own for its whole life). Dropping the handle releases it.
    fn hold_substrate_lock(home: &Path) -> std::fs::File {
        let pid = std::process::id();
        std::fs::write(
            home.join("substrate.pid"),
            format!("{pid}\n{}", home.display()),
        )
        .unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join("substrate.lock"))
            .unwrap();
        lock.lock().unwrap();
        lock
    }

    #[test]
    fn daemon_document_round_trips_offset_pid_and_fallback() {
        let home = scratch("daemon-doc");
        let pf = PortsFile::for_daemon(home.clone(), 100, 0, 4242, true);
        assert!(pf.is_fallback());
        assert_eq!(pf.ports.get("gmcp"), Some(&9190));
        let json = pf.to_json();
        assert_eq!(json["offset"], 100);
        assert_eq!(json["pid"], 4242);
        assert_eq!(json["fallback_from"], 0);
        assert_eq!(json["fallback"], true);
        pf.write_to_home().unwrap();
        assert_eq!(PortsFile::read_from_home(&home), Some(pf));

        let pinned = PortsFile::for_daemon(home.clone(), 300, 300, 7, false);
        assert!(!pinned.is_fallback());
        assert_eq!(pinned.to_json()["fallback"], false);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_document_without_offset_or_pid_still_parses() {
        let home = scratch("legacy-shape");
        std::fs::write(
            home.join("ports.json"),
            r#"{"ports": {"gmcp": 9190, "gemi": 9191}, "tls": false}"#,
        )
        .unwrap();
        let pf = PortsFile::read_from_home(&home).unwrap();
        assert_eq!(pf.offset, 100, "offset is read back from the GMCP port");
        assert_eq!(pf.pid, 0, "a legacy file names no daemon");
        assert_eq!(pf.fallback_from, 100);
        assert!(!pf.is_fallback());
        assert_eq!(
            crate::ports::live_ports_in(&home),
            None,
            "no pid, never live"
        );
        std::fs::write(home.join("ports.json"), r#"{"ports": {}, "offset": 70000}"#).unwrap();
        assert_eq!(
            PortsFile::read_from_home(&home),
            None,
            "a bad offset rejects the file"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn only_the_live_lock_holder_publication_is_trusted() {
        let home = scratch("live");
        let pid = std::process::id();
        PortsFile::for_daemon(home.clone(), 100, 0, pid, false)
            .write_to_home()
            .unwrap();
        assert_eq!(
            crate::ports::live_ports_in(&home),
            None,
            "no substrate lock: nobody is serving this file"
        );

        let lock = hold_substrate_lock(&home);
        // A rewrite (new length) refreshes the cached verdict at once.
        PortsFile::for_daemon(home.clone(), 1000, 0, pid, false)
            .write_to_home()
            .unwrap();
        let live = crate::ports::live_ports_in(&home).expect("live daemon publication");
        assert_eq!(live.offset, 1000);
        assert_eq!(live.pid, pid);
        assert!(live.is_fallback());

        // The record names another pid: not this daemon's file.
        PortsFile::for_daemon(home.clone(), 200, 0, pid.wrapping_add(1), false)
            .write_to_home()
            .unwrap();
        assert_eq!(crate::ports::live_ports_in(&home), None);
        assert!(
            PortsFile::remove_unless_live(&home),
            "a stale file is removed"
        );
        assert!(!home.join("ports.json").exists());

        PortsFile::for_daemon(home.clone(), 100, 0, pid, false)
            .write_to_home()
            .unwrap();
        assert!(!PortsFile::remove_unless_live(&home), "the live file stays");
        assert!(!PortsFile::remove_if_written_by(&home, pid.wrapping_add(1)));
        assert!(PortsFile::remove_if_written_by(&home, pid));
        assert!(!home.join("ports.json").exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_dead_daemon_pid_is_ignored() {
        let home = scratch("dead");
        // Above every real pid on Windows and Linux (pid_max <= 4_194_304).
        let dead = 4_000_000_u32;
        std::fs::write(home.join("substrate.pid"), format!("{dead}\n")).unwrap();
        std::fs::write(home.join("substrate.lock"), format!("{dead}\n")).unwrap();
        PortsFile::for_daemon(home.clone(), 100, 0, dead, false)
            .write_to_home()
            .unwrap();
        assert!(!crate::ports::daemon_lock_live(&home, dead));
        assert_eq!(crate::ports::live_ports_in(&home), None);
        assert!(PortsFile::remove_unless_live(&home));
        let _ = std::fs::remove_dir_all(&home);
    }
}
