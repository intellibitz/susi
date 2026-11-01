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
//! `https_only` (default true): refuse remote plaintext even when no TLS
//! certificate can be offered.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use tokio_rustls::TlsAcceptor;
use tokio_rustls::pki_types::CertificateDer;
use tokio_rustls::pki_types::pem::{self, PemObject};

pub use susi_http_transport::dual_transport::{MaybeTls, negotiate_transport};

/// True when `bind_address` names a loopback-only interface.
pub fn is_loopback(bind_address: &str) -> bool {
    bind_address
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or_else(|_| matches!(bind_address, "localhost" | "localhost."))
}

/// Load a PEM cert/key pair into a `TlsAcceptor`.
pub fn build_acceptor(cert_path: &Path, key_path: &Path) -> std::io::Result<TlsAcceptor> {
    let cert = std::fs::read(cert_path)?;
    let key = std::fs::read(key_path)?;
    tokio_rustls::acceptor_from_pem(&cert, &key)
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
    // Owner-only from creation: a write-then-chmod leaves the private key
    // readable under the umask until the chmod lands (or forever, if it fails).
    crate::susi_config::atomic_write_bytes(&key_path, signing_key.serialize_pem().as_bytes())?;
    crate::susi_config::atomic_write_bytes(&cert_path, cert.pem().as_bytes())?;
    eprintln!(
        "[TLS] Generated self-signed certificate at {} (clients must trust or pin this certificate)",
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
                    "[TLS] Failed to load configured cert/key ({}/{}): {} — TLS is unavailable; remote plaintext is refused by default",
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
                 remote transport policy applies{}",
                e,
                if https_only() {
                    "; https_only refuses remote connections entirely"
                } else {
                    " (plaintext explicitly enabled)"
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
        .unwrap_or(true)
}

/// How long a staged certificate may wait before activation is refused.
/// A certificate staged longer ago than this cannot silently activate on
/// fingerprint match alone (VC-201-077).
pub const MAX_CERT_STAGE_AGE_SECS: u64 = 24 * 60 * 60;

/// How long a retired certificate remains usable in bounded overlap after rotation.
pub const PREV_CERT_OVERLAP_SECS: u64 = 7 * 24 * 60 * 60;

/// Generate a self-signed certificate and private key in PEM format.
pub fn generate_self_signed(sans: &[String]) -> std::io::Result<(String, String)> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(sans.to_vec())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((cert.pem(), signing_key.serialize_pem()))
}

/// Compute SHA-256 fingerprint hex of the first PEM certificate in `cert_pem`.
pub fn cert_fingerprint(cert_pem: &str) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let invalid = |e: pem::Error| {
        if let pem::Error::Io(io) = e {
            io
        } else {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        }
    };
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(invalid)?;
    if let Some(first) = certs.first() {
        Ok(hex::encode(Sha256::digest(first.as_ref())))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no PEM certificates found in slice",
        ))
    }
}

/// Compute SHA-256 fingerprint hex of the first certificate in the PEM file at `cert_path`.
pub fn cert_file_fingerprint(cert_path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let invalid = |e: pem::Error| {
        if let pem::Error::Io(io) = e {
            io
        } else {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        }
    };
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .map_err(invalid)?
        .collect::<Result<_, _>>()
        .map_err(invalid)?;
    if let Some(first) = certs.first() {
        Ok(hex::encode(Sha256::digest(first.as_ref())))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("no PEM certificates in {}", cert_path.display()),
        ))
    }
}

fn read_timestamp(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_timestamp(path: &Path, ts: u64) -> std::io::Result<()> {
    crate::susi_config::atomic_write_bytes(path, ts.to_string().as_bytes())
}

/// Stage a next-epoch transport certificate pair beside `cert.pem` and `key.pem`.
pub fn stage_cert_pair_at(
    tls_dir: &Path,
    cert_pem: &str,
    key_pem: &str,
    now_unix_secs: u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(tls_dir)?;
    let cert_next = tls_dir.join("cert.pem.next");
    let key_next = tls_dir.join("key.pem.next");
    let staged_at = tls_dir.join("cert.pem.next.staged_at");
    crate::susi_config::atomic_write_bytes(&cert_next, cert_pem.as_bytes())?;
    crate::susi_config::atomic_write_bytes(&key_next, key_pem.as_bytes())?;
    write_timestamp(&staged_at, now_unix_secs)?;
    Ok(())
}

/// Activate a staged certificate pair at `tls_dir` iff its fingerprint matches
/// `expected_fingerprint`. On success, the existing certificate rotates to
/// `cert.pem.prev` / `key.pem.prev` with a bounded-overlap expiry, and the staged
/// pair becomes active.
///
/// If the staged files are torn or invalid PEM, or if they have waited past
/// `MAX_CERT_STAGE_AGE_SECS`, activation fails and clears the torn/expired staged files
/// to self-heal interrupted rotations without manual intervention.
pub fn activate_staged_cert_at(
    tls_dir: &Path,
    expected_fingerprint: &str,
    now_unix_secs: u64,
) -> bool {
    let cert_next = tls_dir.join("cert.pem.next");
    let key_next = tls_dir.join("key.pem.next");
    let staged_at_path = tls_dir.join("cert.pem.next.staged_at");

    let cleanup_staged = || {
        let _ = std::fs::remove_file(&cert_next);
        let _ = std::fs::remove_file(&key_next);
        let _ = std::fs::remove_file(&staged_at_path);
    };

    if !cert_next.exists() || !key_next.exists() {
        cleanup_staged();
        return false;
    }

    // Verify valid cert and key can form an acceptor
    if build_acceptor(&cert_next, &key_next).is_err() {
        cleanup_staged();
        return false;
    }

    // Verify fingerprint match
    match cert_file_fingerprint(&cert_next) {
        Ok(fp) if fp == expected_fingerprint => {}
        _ => {
            return false;
        }
    }

    // Verify age
    let staged_at = read_timestamp(&staged_at_path).unwrap_or(0);
    if now_unix_secs.saturating_sub(staged_at) > MAX_CERT_STAGE_AGE_SECS {
        cleanup_staged();
        return false;
    }

    let cert_active = tls_dir.join("cert.pem");
    let key_active = tls_dir.join("key.pem");
    let cert_prev = tls_dir.join("cert.pem.prev");
    let key_prev = tls_dir.join("key.pem.prev");
    let prev_expires_at = tls_dir.join("cert.pem.prev.expires_at");

    let had_active = cert_active.exists() && key_active.exists();
    if had_active {
        // Prepare prev files first so active files remain untouched if this fails
        if std::fs::copy(&cert_active, &cert_prev).is_err()
            || std::fs::copy(&key_active, &key_prev).is_err()
            || write_timestamp(&prev_expires_at, now_unix_secs + PREV_CERT_OVERLAP_SECS).is_err()
        {
            let _ = std::fs::remove_file(&cert_prev);
            let _ = std::fs::remove_file(&key_prev);
            let _ = std::fs::remove_file(&prev_expires_at);
            return false;
        }
    }

    // Atomically replace active files with staged files
    if std::fs::rename(&cert_next, &cert_active).is_err() {
        if had_active {
            let _ = std::fs::remove_file(&cert_prev);
            let _ = std::fs::remove_file(&key_prev);
            let _ = std::fs::remove_file(&prev_expires_at);
        }
        return false;
    }
    if std::fs::rename(&key_next, &key_active).is_err() {
        // Rollback cert_active so no mismatched certificate/key pair is left behind
        if had_active {
            let _ = std::fs::copy(&cert_prev, &cert_active);
        } else {
            let _ = std::fs::remove_file(&cert_active);
        }
        return false;
    }
    let _ = std::fs::remove_file(&staged_at_path);

    true
}

/// Prior-epoch certificate pair, retained after rotation for bounded overlap.
/// Returns None when no prior certificate exists, the overlap window has elapsed,
/// or the retired certificate was explicitly revoked.
pub fn prev_cert_at(tls_dir: &Path, now_unix_secs: u64) -> Option<(PathBuf, PathBuf)> {
    let cert_prev = tls_dir.join("cert.pem.prev");
    let key_prev = tls_dir.join("key.pem.prev");
    let expires_at_path = tls_dir.join("cert.pem.prev.expires_at");

    if !cert_prev.exists() || !key_prev.exists() {
        return None;
    }

    let expires_at = read_timestamp(&expires_at_path)?;
    if now_unix_secs >= expires_at {
        return None;
    }

    Some((cert_prev, key_prev))
}

/// Explicitly revoke the retired transport certificate before its overlap window
/// elapses. After revocation, `prev_cert_at` returns None immediately.
pub fn revoke_prev_cert_at(tls_dir: &Path) -> bool {
    let cert_prev = tls_dir.join("cert.pem.prev");
    let key_prev = tls_dir.join("key.pem.prev");
    let expires_at_path = tls_dir.join("cert.pem.prev.expires_at");

    let r1 = std::fs::remove_file(&cert_prev).is_ok();
    let r2 = std::fs::remove_file(&key_prev).is_ok();
    let _ = std::fs::remove_file(&expires_at_path);

    r1 || r2
}
