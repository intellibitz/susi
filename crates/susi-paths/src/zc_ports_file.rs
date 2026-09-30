//! Stable discovery file (`ports.json`) for every client.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Snapshot published for clients that must not hardcode 9090–9094.
#[derive(Debug, Clone, PartialEq)]
pub struct PortsFile {
    pub path: PathBuf,
    pub ports: BTreeMap<String, u16>,
    pub token_path: PathBuf,
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
            tls_enabled,
        }
    }

    /// Serialize to the JSON clients read.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ports": self.ports,
            "token_path": self.token_path.to_string_lossy(),
            "tls": self.tls_enabled,
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
        let v = pf.to_json();
        assert_eq!(v["tls"], true);
        assert!(v["token_path"].as_str().unwrap().ends_with("token"));
    }
}
