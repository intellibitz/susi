//! Local TLS cert generation defaults.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsAuto {
    pub cert_path: String,
    pub key_path: String,
    pub generated: bool,
}

#[must_use]
pub fn ensure_local_tls(home: &str, already: bool) -> TlsAuto {
    TlsAuto {
        cert_path: format!("{home}/tls/cert.pem"),
        key_path: format!("{home}/tls/key.pem"),
        generated: !already,
    }
}

#[cfg(test)]
mod zc_tls_auto_tests {
    use super::*;

    #[test]
    fn zc_tls_auto_generates_when_missing() {
        let t = ensure_local_tls("/tmp/susi", false);
        assert!(t.generated);
        assert!(t.cert_path.ends_with("cert.pem"));
        assert!(!ensure_local_tls("/tmp/susi", true).generated);
    }
}
