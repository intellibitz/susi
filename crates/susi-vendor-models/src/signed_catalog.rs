//! Signed catalog-update channel: catalog payloads (models, quants, cost
//! tiers — and, for the ecosystem knowledge base, `eco_signed_kb`) arrive as
//! signed bundles, are verified against trusted publisher keys before they
//! touch disk, reject version downgrades/replays, and keep a rollback copy.
//!
//! Wire format — one JSON document:
//!
//! ```text
//! {"channel","version","payload","signature","key_id"}
//! ```
//!
//! The signature is Ed25519 over
//! `b"signed-catalog-v1\0" || channel || 0x00 || version_be64 || payload`,
//! hex-encoded. `key_id` names the publisher for audit; trust is the caller's
//! [`VerifyingKey`] set, not the claimed id.
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult, ResultExt as Context};

const DOMAIN: &[u8] = b"signed-catalog-v1";
const CATALOG_FILE: &str = "catalog.json";
const CATALOG_PREV: &str = "catalog.prev.json";
const MANIFEST: &str = "catalog.manifest.json";
const MANIFEST_PREV: &str = "catalog.manifest.prev.json";

/// A signed catalog update as it arrives on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogBundle {
    /// Logical channel ("models", "ecosystem-kb", …) — part of the signature.
    pub channel: String,
    /// Monotonic catalog version; installs reject `<=` the installed one.
    pub version: u64,
    /// The catalog payload itself (JSON text — opaque to the channel).
    pub payload: String,
    /// Hex Ed25519 signature over the domain-separated preimage.
    pub signature: String,
    /// Publisher key id, for audit output only (trust is the key set).
    pub key_id: String,
}

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallOutcome {
    pub version: u64,
    /// Version the rollback copy now holds, if a previous catalog existed.
    pub rollback_version: Option<u64>,
}

fn manifest(dir: &Path) -> PathBuf {
    dir.join(MANIFEST)
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    channel: String,
    version: u64,
    key_id: String,
}

fn signed_preimage(bundle: &CatalogBundle) -> Vec<u8> {
    let mut msg = Vec::with_capacity(bundle.payload.len() + 64);
    msg.extend_from_slice(DOMAIN);
    msg.push(0);
    msg.extend_from_slice(bundle.channel.as_bytes());
    msg.push(0);
    msg.extend_from_slice(&bundle.version.to_be_bytes());
    msg.extend_from_slice(bundle.payload.as_bytes());
    msg
}

/// Sign a catalog payload into a wire-ready bundle.
#[must_use]
pub fn sign_bundle(
    channel: &str,
    version: u64,
    payload: &str,
    key: &SigningKey,
    key_id: &str,
) -> CatalogBundle {
    let unsigned = CatalogBundle {
        channel: channel.to_string(),
        version,
        payload: payload.to_string(),
        signature: String::new(),
        key_id: key_id.to_string(),
    };
    let sig = key.sign(&signed_preimage(&unsigned));
    CatalogBundle {
        signature: hex::encode(sig.to_bytes()),
        ..unsigned
    }
}

/// Verify a bundle's signature against the trusted publisher keys.
/// Tampering, malformed hex, wrong keys — all rejected identically.
///
/// # Errors
/// `EaiError::config` when the signature does not verify under any trusted key.
pub fn verify_bundle(bundle: &CatalogBundle, trusted: &[VerifyingKey]) -> EaiResult<()> {
    let sig_bytes = hex::decode(&bundle.signature)
        .map_err(|e| EaiError::config(format!("malformed signature hex: {e}")))?;
    let arr: &[u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| EaiError::config("signature is not 64 bytes".to_string()))?;
    let sig = Signature::from_bytes(arr);
    let msg = signed_preimage(bundle);
    if trusted.iter().any(|k| k.verify(&msg, &sig).is_ok()) {
        Ok(())
    } else {
        Err(EaiError::config("signature verification failed"))
    }
}

/// Installed catalog version, or 0 when nothing has been installed.
#[must_use]
pub fn current_version(dir: &Path) -> u64 {
    std::fs::read_to_string(manifest(dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
        .map(|m| m.version)
        .unwrap_or(0)
}

/// Verify, then install: signature → downgrade check → rollback copy →
/// atomic write. A bundle at `version <=` the installed one is refused.
///
/// # Errors
/// `EaiError::config` on bad signature, downgrade, or malformed bundle;
/// `EaiError::io` on filesystem failure.
pub fn install(
    dir: &Path,
    bundle: &CatalogBundle,
    trusted: &[VerifyingKey],
) -> EaiResult<InstallOutcome> {
    verify_bundle(bundle, trusted)?;
    let current = current_version(dir);
    if bundle.version <= current {
        return Err(EaiError::config(format!(
            "catalog version {} does not exceed installed {current} — refusing downgrade/replay",
            bundle.version
        )));
    }
    std::fs::create_dir_all(dir).context("create catalog directory")?;

    // Rollback: keep the previous payload + manifest beside the new ones.
    let prev = dir.join(CATALOG_FILE);
    let rollback_version = if prev.exists() {
        std::fs::copy(&prev, dir.join(CATALOG_PREV)).context("snapshot rollback payload")?;
        std::fs::copy(manifest(dir), dir.join(MANIFEST_PREV))
            .context("snapshot rollback manifest")?;
        Some(current)
    } else {
        None
    };

    susi_config::atomic_write_bytes(&prev, bundle.payload.as_bytes())
        .context("write catalog payload")?;
    let m = Manifest {
        channel: bundle.channel.clone(),
        version: bundle.version,
        key_id: bundle.key_id.clone(),
    };
    susi_config::atomic_write_json_pretty(&manifest(dir), &m).context("write catalog manifest")?;
    Ok(InstallOutcome {
        version: bundle.version,
        rollback_version,
    })
}

/// Restore the rollback copy. Removes the current payload entirely when
/// there is no rollback — after a failed first install there is nothing to
/// roll back to.
///
/// # Errors
/// `EaiError::config` when no rollback copy exists.
pub fn rollback(dir: &Path) -> EaiResult<u64> {
    let prev_payload = dir.join(CATALOG_PREV);
    let prev_manifest = dir.join(MANIFEST_PREV);
    if !(prev_payload.exists() && prev_manifest.exists()) {
        return Err(EaiError::config("no rollback copy is installed"));
    }
    std::fs::copy(&prev_payload, dir.join(CATALOG_FILE)).context("restore catalog payload")?;
    std::fs::copy(&prev_manifest, manifest(dir)).context("restore catalog manifest")?;
    std::fs::remove_file(&prev_payload).ok();
    std::fs::remove_file(&prev_manifest).ok();
    Ok(current_version(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn trusted() -> Vec<VerifyingKey> {
        vec![key().verifying_key()]
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("susi-signed-catalog-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn signed_catalog_updates_roundtrip_verifies() {
        let b = sign_bundle("models", 3, r#"{"models":["a"]}"#, &key(), "release");
        assert!(verify_bundle(&b, &trusted()).is_ok());
    }

    #[test]
    fn signed_catalog_updates_tampered_payload_or_version_rejected() {
        let b = sign_bundle("models", 3, r#"{"models":["a"]}"#, &key(), "release");
        for tampered in [
            CatalogBundle {
                payload: r#"{"models":["evil"]}"#.into(),
                ..b.clone()
            },
            CatalogBundle {
                version: 4,
                ..b.clone()
            },
            CatalogBundle {
                channel: "other".into(),
                ..b.clone()
            },
            CatalogBundle {
                signature: "00".repeat(64),
                ..b.clone()
            },
        ] {
            assert!(verify_bundle(&tampered, &trusted()).is_err());
        }
    }

    #[test]
    fn signed_catalog_updates_untrusted_key_rejected() {
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let b = sign_bundle("models", 3, "{}", &other, "stranger");
        assert!(verify_bundle(&b, &trusted()).is_err());
    }

    #[test]
    fn signed_catalog_updates_install_then_downgrade_and_replay_rejected() {
        let dir = tmpdir("downgrade");
        let b2 = sign_bundle("models", 2, "v2", &key(), "release");
        assert_eq!(install(&dir, &b2, &trusted()).unwrap().version, 2);
        assert!(dir.join(CATALOG_FILE).exists());

        let b1 = sign_bundle("models", 1, "v1", &key(), "release");
        assert!(install(&dir, &b1, &trusted()).is_err());
        assert!(install(&dir, &b2, &trusted()).is_err()); // replay
        assert_eq!(current_version(&dir), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signed_catalog_updates_rollback_restores_previous() {
        let dir = tmpdir("rollback");
        install(
            &dir,
            &sign_bundle("models", 1, "v1", &key(), "r"),
            &trusted(),
        )
        .unwrap();
        let out = install(
            &dir,
            &sign_bundle("models", 2, "v2", &key(), "r"),
            &trusted(),
        )
        .unwrap();
        assert_eq!(out.rollback_version, Some(1));
        assert_eq!(
            std::fs::read_to_string(dir.join(CATALOG_FILE)).unwrap(),
            "v2"
        );

        assert_eq!(rollback(&dir).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.join(CATALOG_FILE)).unwrap(),
            "v1"
        );
        // Second rollback has nothing to restore.
        assert!(rollback(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signed_catalog_updates_unsigned_install_never_touches_disk() {
        let dir = tmpdir("unsigned");
        let mut b = sign_bundle("models", 1, "v1", &key(), "r");
        b.payload = "tampered".into();
        assert!(install(&dir, &b, &trusted()).is_err());
        assert!(!dir.join(CATALOG_FILE).exists());
    }
}
