#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-077: Rotate secrets and certificates without silent downgrade.
//!
//! Mastery target:
//! "Roll credentials and transport certificates with bounded overlap and explicit revocation;
//!  interrupted rotation recovers and expired credentials cannot trigger plaintext or unauthenticated fallback."
//!
//! Criteria verified:
//! 1. Credential (cluster.key) rotation:
//!    - Bounded overlap: old key moves to `cluster.key.prev` and expires out of `prev_key_at` after bounded window.
//!    - Explicit revocation: `revoke_prev_key_at` drops retired key immediately before window elapses.
//!    - Staged key expiry: staged key past age bound cannot activate on fingerprint match alone.
//!    - Interrupted rotation recovery: torn staged files self-heal without outside intervention.
//! 2. Transport certificate rotation:
//!    - Bounded overlap: staged cert activates to `cert.pem`, moving active to `cert.pem.prev` with `expires_at`.
//!    - Bounded overlap retrieval: `prev_cert_at` returns previous cert while within window, `None` when expired.
//!    - Explicit revocation: `revoke_prev_cert_at` drops `cert.pem.prev` and expiry immediately.
//!    - Staged certificate expiry: staged cert past `MAX_CERT_STAGE_AGE_SECS` refuses activation and self-clears.
//!    - Fingerprint validation: activation strictly requires matching SHA-256 fingerprint.
//!    - Interrupted rotation recovery: corrupt/torn staged files fail activation and are cleaned up, self-healing future staging.
//! 3. Fallback prevention:
//!    - Expired/absent TLS certificates under `https_only` / `require_tls_remote` strictly refuse remote plaintext.
//!    - TLS negotiation errors never trigger plaintext or unauthenticated fallback.

mod common;

use common::HomeGuard;
use std::fs;
use std::path::PathBuf;
use susi_config::cluster_key;
use susi_daemon::tls::{self, negotiate_transport};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

fn temp_tls_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vc077_tls_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|x| x.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn vc_201_077_mastery_cluster_key_bounded_overlap_and_explicit_revocation() {
    let _guard = HomeGuard::new("vc077_ck_overlap");
    let dir = _guard.config();

    let old_key: [u8; 32] = [0x11; 32];
    let new_key: [u8; 32] = [0x22; 32];

    // Install initial active key
    fs::write(dir.join("cluster.key"), hex::encode(old_key)).unwrap();

    // Stage new key and activate
    assert!(cluster_key::stage_key_to(
        &new_key,
        &dir.join("cluster.key.next")
    ));
    let fp = cluster_key::key_fingerprint(&new_key);
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));

    // Current key must now be new_key
    let active_bytes = fs::read_to_string(dir.join("cluster.key")).unwrap();
    assert_eq!(active_bytes.trim(), hex::encode(new_key));

    // Retired key is in prev_key within bounded overlap
    assert_eq!(cluster_key::prev_key_at(&dir, 0), Some(old_key));

    // Once overlap window expires, prev_key_at refuses to return the expired key
    assert_eq!(cluster_key::prev_key_at(&dir, u64::MAX), None);

    // Explicit revocation drops the retired key immediately regardless of remaining overlap
    assert!(cluster_key::revoke_prev_key_at(&dir));
    assert_eq!(cluster_key::prev_key_at(&dir, 0), None);
    assert!(!dir.join("cluster.key.prev").exists());
}

#[test]
fn vc_201_077_mastery_cluster_key_staged_expiry_and_torn_self_heal() {
    let _guard = HomeGuard::new("vc077_ck_expiry");
    let dir = _guard.config();

    let old_key: [u8; 32] = [0x33; 32];
    let ancient_key: [u8; 32] = [0x44; 32];
    fs::write(dir.join("cluster.key"), hex::encode(old_key)).unwrap();

    // Stage key and backdate .staged_at timestamp to simulate ancient stage
    assert!(cluster_key::stage_key_to(
        &ancient_key,
        &dir.join("cluster.key.next")
    ));
    let fp = cluster_key::key_fingerprint(&ancient_key);
    fs::write(dir.join("cluster.key.next.staged_at"), "0").unwrap();

    // Activation must fail due to age bound
    assert!(
        !cluster_key::activate_staged_key_at(&fp, &dir),
        "expired staged key must not activate"
    );

    // Torn stage test: corrupt next key file
    fs::write(dir.join("cluster.key.next"), "invalid-hex-corrupt-data").unwrap();
    assert!(!cluster_key::activate_staged_key_at("any-fp", &dir));
    assert!(
        !dir.join("cluster.key.next").exists(),
        "torn staged file must self-heal by being removed"
    );

    // After self-healing, a fresh stage activates cleanly
    let fresh_key: [u8; 32] = [0x55; 32];
    assert!(cluster_key::stage_key_to(
        &fresh_key,
        &dir.join("cluster.key.next")
    ));
    let fresh_fp = cluster_key::key_fingerprint(&fresh_key);
    assert!(cluster_key::activate_staged_key_at(&fresh_fp, &dir));
}

#[test]
fn vc_201_077_mastery_transport_cert_rotation_bounded_overlap() {
    let tls_dir = temp_tls_dir("overlap");

    // Generate initial certificate
    let (cert1_pem, key1_pem) =
        tls::generate_self_signed(&["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
    fs::write(tls_dir.join("cert.pem"), &cert1_pem).unwrap();
    fs::write(tls_dir.join("key.pem"), &key1_pem).unwrap();

    // Verify initial acceptor can be built
    assert!(tls::build_acceptor(&tls_dir.join("cert.pem"), &tls_dir.join("key.pem")).is_ok());

    // Generate new certificate to stage
    let (cert2_pem, key2_pem) =
        tls::generate_self_signed(&["localhost".to_string(), "node2.local".to_string()]).unwrap();
    let now = 1_000_000;
    tls::stage_cert_pair_at(&tls_dir, &cert2_pem, &key2_pem, now).unwrap();

    // Calculate fingerprint of staged cert
    let fp2 = tls::cert_fingerprint(&cert2_pem).unwrap();

    // Activate staged cert
    assert!(tls::activate_staged_cert_at(&tls_dir, &fp2, now));

    // Active cert should now match fp2
    assert_eq!(
        tls::cert_file_fingerprint(&tls_dir.join("cert.pem")).unwrap(),
        fp2
    );

    // Previous cert should exist in bounded overlap
    let prev = tls::prev_cert_at(&tls_dir, now);
    assert!(
        prev.is_some(),
        "previous cert must be available within overlap window"
    );
    let (prev_cert, prev_key) = prev.unwrap();
    assert_eq!(
        tls::cert_file_fingerprint(&prev_cert).unwrap(),
        tls::cert_fingerprint(&cert1_pem).unwrap()
    );
    assert!(prev_key.exists());

    // When overlap window passes, prev_cert_at returns None
    let expired_time = now + tls::PREV_CERT_OVERLAP_SECS + 1;
    assert!(
        tls::prev_cert_at(&tls_dir, expired_time).is_none(),
        "expired transport cert must not be usable after overlap window"
    );

    let _ = fs::remove_dir_all(&tls_dir);
}

#[test]
fn vc_201_077_mastery_transport_cert_explicit_revocation() {
    let tls_dir = temp_tls_dir("revocation");

    let (cert1_pem, key1_pem) = tls::generate_self_signed(&["localhost".to_string()]).unwrap();
    fs::write(tls_dir.join("cert.pem"), &cert1_pem).unwrap();
    fs::write(tls_dir.join("key.pem"), &key1_pem).unwrap();

    let (cert2_pem, key2_pem) = tls::generate_self_signed(&["localhost".to_string()]).unwrap();
    let now = 500_000;
    tls::stage_cert_pair_at(&tls_dir, &cert2_pem, &key2_pem, now).unwrap();
    let fp2 = tls::cert_fingerprint(&cert2_pem).unwrap();
    assert!(tls::activate_staged_cert_at(&tls_dir, &fp2, now));

    // Within window, prev cert is available
    assert!(tls::prev_cert_at(&tls_dir, now).is_some());

    // Explicit revocation drops retired cert immediately
    assert!(tls::revoke_prev_cert_at(&tls_dir));
    assert!(
        tls::prev_cert_at(&tls_dir, now).is_none(),
        "revoked transport cert must be immediately unusable"
    );
    assert!(!tls_dir.join("cert.pem.prev").exists());
    assert!(!tls_dir.join("key.pem.prev").exists());

    let _ = fs::remove_dir_all(&tls_dir);
}

#[test]
fn vc_201_077_mastery_transport_cert_staged_expiry_and_fingerprint_mismatch() {
    let tls_dir = temp_tls_dir("expiry");

    let (cert1_pem, key1_pem) = tls::generate_self_signed(&["localhost".to_string()]).unwrap();
    fs::write(tls_dir.join("cert.pem"), &cert1_pem).unwrap();
    fs::write(tls_dir.join("key.pem"), &key1_pem).unwrap();

    let (cert2_pem, key2_pem) = tls::generate_self_signed(&["localhost".to_string()]).unwrap();
    let staged_time = 100_000;
    tls::stage_cert_pair_at(&tls_dir, &cert2_pem, &key2_pem, staged_time).unwrap();
    let fp2 = tls::cert_fingerprint(&cert2_pem).unwrap();

    // Fingerprint mismatch refuses activation and does NOT delete staged cert
    assert!(!tls::activate_staged_cert_at(
        &tls_dir,
        "wrong-fp",
        staged_time
    ));
    assert!(tls_dir.join("cert.pem.next").exists());

    // Staged cert past MAX_CERT_STAGE_AGE_SECS refuses activation and self-clears
    let too_late = staged_time + tls::MAX_CERT_STAGE_AGE_SECS + 10;
    assert!(
        !tls::activate_staged_cert_at(&tls_dir, &fp2, too_late),
        "stale staged cert must not activate"
    );
    assert!(
        !tls_dir.join("cert.pem.next").exists(),
        "stale staged cert files must be cleared"
    );

    let _ = fs::remove_dir_all(&tls_dir);
}

#[test]
fn vc_201_077_mastery_transport_cert_torn_stage_self_heals() {
    let tls_dir = temp_tls_dir("torn");

    let (cert1_pem, key1_pem) = tls::generate_self_signed(&["localhost".to_string()]).unwrap();
    fs::write(tls_dir.join("cert.pem"), &cert1_pem).unwrap();
    fs::write(tls_dir.join("key.pem"), &key1_pem).unwrap();

    // Simulate crash mid-stage: invalid/truncated files
    fs::write(tls_dir.join("cert.pem.next"), "NOT_A_VALID_PEM_CERTIFICATE").unwrap();
    fs::write(tls_dir.join("key.pem.next"), "NOT_A_VALID_PRIVATE_KEY").unwrap();

    // Activation fails and self-clears torn files
    assert!(!tls::activate_staged_cert_at(
        &tls_dir,
        "any-fingerprint",
        100
    ));
    assert!(
        !tls_dir.join("cert.pem.next").exists(),
        "torn staged cert must be removed"
    );
    assert!(
        !tls_dir.join("key.pem.next").exists(),
        "torn staged key must be removed"
    );

    // Subsequent valid stage and activation succeeds without manual cleanup
    let (cert2_pem, key2_pem) = tls::generate_self_signed(&["localhost".to_string()]).unwrap();
    tls::stage_cert_pair_at(&tls_dir, &cert2_pem, &key2_pem, 100).unwrap();
    let fp2 = tls::cert_fingerprint(&cert2_pem).unwrap();
    assert!(tls::activate_staged_cert_at(&tls_dir, &fp2, 100));
    assert_eq!(
        tls::cert_file_fingerprint(&tls_dir.join("cert.pem")).unwrap(),
        fp2
    );

    let _ = fs::remove_dir_all(&tls_dir);
}

#[tokio::test]
async fn vc_201_077_mastery_no_plaintext_or_unauthenticated_fallback_on_expired_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Case 1: Remote client sends plaintext when require_tls_remote is true.
    // The connection must be dropped (None), never accepted as plaintext.
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // remote = true, tls = None (expired/absent cert), require_tls_remote = true
        negotiate_transport(stream, true, None, true, "[TEST]").await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    drop(client);

    let result = server_task.await.unwrap();
    assert!(
        result.is_none(),
        "plaintext connection must be refused when require_tls_remote is set"
    );

    // Case 2: Remote client sends TLS ClientHello (first byte 0x16), but no TLS certificate
    // can be offered (tls = None due to expired or missing credentials).
    // Negotiation must return None — no silent downgrade to plaintext HTTP!
    let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener2.local_addr().unwrap();

    let server_task2 = tokio::spawn(async move {
        let (stream, _) = listener2.accept().await.unwrap();
        negotiate_transport(stream, true, None, true, "[TEST]").await
    });

    let mut client2 = TcpStream::connect(addr2).await.unwrap();
    // 0x16 is TLS Handshake record type
    client2
        .write_all(&[0x16, 0x03, 0x01, 0x00, 0x05, 0x01, 0x00, 0x00, 0x01, 0x00])
        .await
        .unwrap();
    drop(client2);

    let result2 = server_task2.await.unwrap();
    assert!(
        result2.is_none(),
        "missing or expired TLS certificate must drop connection, not fallback to plaintext"
    );
}
