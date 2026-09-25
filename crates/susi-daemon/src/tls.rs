//! Optional TLS for the public HTTP endpoints (GEMI/GMCP/A2A).
//!
//! Transport contract (first-byte sniffing on every socket):
//!   - A TLS ClientHello (`0x16`) upgrades to TLS whenever an acceptor is
//!     configured; anything else is plain HTTP. Loopback and remote sockets
//!     behave identically — sniffing keeps the surface compatible with
//!     TLS-terminating reverse proxies and mixed-version peers.
//!   - Plaintext policy is peer-based: off-host plaintext is refused only
//!     when `https_only` is set; loopback always passes so internal
//!     `http://127.0.0.1` callers are never broken.
//!
//! Certificate resolution order:
//!   1. `tls_cert_path` + `tls_key_path` in config.json (PEM pair)
//!   2. non-loopback `bind_address` → auto-generate (or reuse) a self-signed
//!      cert persisted under `<global_dir>/tls/`
//!   3. loopback bind with no cert → `None` (plain HTTP only)
//!
//! `https_only` (default false): refuse remote plaintext even when no TLS
//! certificate can be offered.

use std::io::BufReader;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// True when `bind_address` names a loopback-only interface.
pub fn is_loopback(bind_address: &str) -> bool {
    bind_address
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or_else(|_| matches!(bind_address, "localhost" | "localhost."))
}

/// Load a PEM cert/key pair into a `TlsAcceptor`.
fn build_acceptor(cert_path: &Path, key_path: &Path) -> std::io::Result<TlsAcceptor> {
    let mut cert_reader = BufReader::new(std::fs::File::open(cert_path)?);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<_, _>>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if certs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("no PEM certificates in {}", cert_path.display()),
        ));
    }
    let mut key_reader = BufReader::new(std::fs::File::open(key_path)?);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("no PEM private key in {}", key_path.display()),
            )
        })?;
    // Exactly one provider may be feature-unified across the workspace —
    // install it explicitly so `builder()` never panics on ambiguity.
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Generate (or reuse) a self-signed certificate under `tls_dir`. The SANs
/// cover localhost, the bind address, and the OS hostname so the cert is
/// usable for both `https://localhost` and `https://<host>` callers.
fn self_signed_pair(tls_dir: &Path, bind_address: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");
    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }
    std::fs::create_dir_all(tls_dir)?;
    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if bind_address != "0.0.0.0" && bind_address != "::" {
        sans.push(bind_address.to_string());
    }
    if let Ok(host) = std::env::var("HOSTNAME")
        && !host.is_empty()
    {
        sans.push(host);
    }
    let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(sans)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&cert_path, cert.pem())?;
    std::fs::write(&key_path, signing_key.serialize_pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    eprintln!(
        "[TLS] Generated self-signed certificate at {} (clients: `curl -k` or pin the cert)",
        cert_path.display()
    );
    Ok((cert_path, key_path))
}

/// Resolve the TLS acceptor for the public endpoints. Returns `None` when no
/// certificate is configured and the bind is loopback-only.
pub fn endpoint_acceptor(bind_address: &str, global_dir: &Path) -> Option<TlsAcceptor> {
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let cert_path = cfg.get::<String>("tls_cert_path").unwrap_or_default();
    let key_path = cfg.get::<String>("tls_key_path").unwrap_or_default();
    if !cert_path.is_empty() && !key_path.is_empty() {
        match build_acceptor(Path::new(&cert_path), Path::new(&key_path)) {
            Ok(acceptor) => {
                eprintln!("[TLS] Loaded configured certificate from {}", cert_path);
                return Some(acceptor);
            }
            Err(e) => {
                eprintln!(
                    "[TLS] Failed to load configured cert/key ({}/{}): {} — endpoints stay HTTP-only",
                    cert_path, key_path, e
                );
                return None;
            }
        }
    }
    if is_loopback(bind_address) {
        return None;
    }
    let tls_dir = global_dir.join("tls");
    match self_signed_pair(&tls_dir, bind_address).and_then(|(c, k)| build_acceptor(&c, &k)) {
        Ok(acceptor) => Some(acceptor),
        Err(e) => {
            eprintln!(
                "[TLS] Self-signed certificate unavailable: {} — \
                 remote peers get plaintext HTTP{}",
                e,
                if https_only() {
                    "; https_only refuses remote connections entirely"
                } else {
                    " (https_only is unset)"
                }
            );
            None
        }
    }
}

/// Whether non-loopback peers must speak TLS (`https_only` config key).
pub fn https_only() -> bool {
    crate::susi_sandbox::manager::SusiConfig::load_global()
        .unwrap_or_default()
        .get::<bool>("https_only")
        .unwrap_or(false)
}
