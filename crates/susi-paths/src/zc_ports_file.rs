//! Stable discovery file (`ports.json`) for every client.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Snapshot published for clients that must not hardcode 9090–9094.
#[derive(Debug, Clone, PartialEq)]
pub struct PortsFile {
    pub path: PathBuf,
    pub ports: BTreeMap<String, u16>,
    pub token_path: PathBuf,
    pub api_token_path: PathBuf,
    pub tls_enabled: bool,
}

impl PortsFile {
    /// Build the discovery document for `home` (typically `~/.susi`).
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
            path: home.join("ports.json"),
            ports,
            token_path: home.join("token"),
            api_token_path: home.join("api_token"),
            tls_enabled,
        }
    }

    /// Serialize to the JSON clients read.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ports": self.ports,
            "token_path": self.token_path.to_string_lossy(),
            "api_token_path": self.api_token_path.to_string_lossy(),
            "tls": self.tls_enabled,
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
                .unwrap_or("ports.json"),
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
    #[must_use]
    pub fn read_from_home(home: &Path) -> Option<Self> {
        let path = home.join("ports.json");
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
        Some(Self {
            path,
            ports,
            token_path,
            api_token_path,
            tls_enabled,
        })
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
}
