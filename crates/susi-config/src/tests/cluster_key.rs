//! Tests for `cluster_key` — kept out of the canonical source so crates that
//! `#[path]`-mount it never compile or run them.

use crate::cluster_key::*;
use std::path::PathBuf;

fn set_key_env() -> TempHomeGuard<'static> {
    // Serialize env mutation within this test binary.
    let guard = crate::env_test_lock();
    let tmp = std::env::temp_dir().join(format!(
        "susi_ck_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Pre-create `.susi` so SusiDirs picks the deterministic legacy path.
    let _ = std::fs::create_dir_all(tmp.join(".susi"));
    unsafe {
        std::env::set_var("HOME", &tmp);
        std::env::set_var("USERPROFILE", &tmp);
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("xdg"));
    }
    TempHomeGuard { _guard: guard, tmp }
}

struct TempHomeGuard<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
    tmp: PathBuf,
}
impl Drop for TempHomeGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

#[test]
fn signed_ping_pong_roundtrip_verifies() {
    let _t = set_key_env();
    let (ping, nonce) = signed_ping("CORE,GPU", 7, "00ff").expect("signed ping");
    let (pinger_id, caps, checksum, bloom, echoed, wants_v2) =
        verify_signed_ping(&ping).expect("verify ping");
    assert!(!wants_v2);
    assert!(pinger_id.starts_with("susi-node-"));
    assert_eq!(
        (caps.as_str(), checksum, bloom.as_str()),
        ("CORE,GPU", 7, "00ff")
    );
    assert_eq!(echoed, nonce);
    let roster = vec![
        ("peer-b".to_string(), "10.0.0.2:9090".to_string()),
        ("peer-c".to_string(), "10.0.0.3:9090".to_string()),
    ];
    let pong = signed_pong("susi-daemon-node", 0, "abcd", &echoed, &roster).expect("signed pong");
    let (node_id, csum, pbloom, gossip, pubkey, bind_sig) =
        verify_signed_pong(&pong, &nonce).expect("verify pong");
    assert!(pubkey.is_empty());
    assert!(bind_sig.is_empty());
    assert_eq!(
        (node_id.as_str(), csum, pbloom.as_str()),
        ("susi-daemon-node", 0, "abcd")
    );
    assert_eq!(gossip, roster);
}

#[test]
fn pong_with_wrong_nonce_or_mac_is_rejected() {
    let _t = set_key_env();
    let (ping, nonce) = signed_ping("CORE", 1, "00").unwrap();
    let (_, _, _, _, echoed, _) = verify_signed_ping(&ping).unwrap();
    let pong = signed_pong("peer", 0, "00", &echoed, &[]).unwrap();
    // Wrong expected nonce -> reject.
    assert!(verify_signed_pong(&pong, "deadbeef").is_none());
    // Tampered mac -> reject.
    let tampered = pong.replacen(":00:", ":11:", 1);
    assert!(verify_signed_pong(&tampered, &nonce).is_none());
    // Unsigned legacy pong never verifies.
    assert!(verify_signed_pong("SUSI_PONG:daemon:0:", &nonce).is_none());
}

#[test]
fn legacy_six_field_pong_still_verifies_with_empty_roster() {
    let _t = set_key_env();
    let (ping, nonce) = signed_ping("CORE", 1, "00").unwrap();
    let (_, _, _, _, echoed, _) = verify_signed_ping(&ping).unwrap();
    // Reconstruct a pre-gossip pong: no roster field, MAC over the
    // original five fields — older daemons still handshake cleanly.
    let key = cluster_key().unwrap();
    let mac = mac_tag(&key, &["pong", "old-peer", "0", "00", &echoed]);
    let legacy = format!("SUSI_PONG_SIG:old-peer:0:00:{echoed}:{mac}");
    let (node_id, _, _, roster, _, _) = verify_signed_pong(&legacy, &nonce).expect("legacy pong");
    assert_eq!(node_id, "old-peer");
    assert!(roster.is_empty());
}

#[test]
fn legacy_six_field_ping_verifies_with_empty_node_id() {
    let _t = set_key_env();
    // Reconstruct a pre-identity ping: no node_id field, MAC over the
    // original four fields — older daemons still handshake cleanly.
    let key = cluster_key().unwrap();
    let mac = mac_tag(&key, &["ping", "CORE", "1", "00", "noncenonce"]);
    let legacy = format!("SUSI_PING_SIG:CORE:1:00:noncenonce:{mac}");
    let (pinger_id, caps, checksum, bloom, echoed, _) =
        verify_signed_ping(&legacy).expect("legacy ping");
    assert!(pinger_id.is_empty());
    assert_eq!(
        (caps.as_str(), checksum, bloom.as_str(), echoed.as_str()),
        ("CORE", 1, "00", "noncenonce")
    );
}

#[test]
fn node_id_persists_and_is_wire_safe() {
    let _t = set_key_env();
    let first = node_id().expect("node id");
    assert!(first.starts_with("susi-node-"));
    // Second call reads back the same identity — identity is stable.
    assert_eq!(node_id().as_deref(), Some(first.as_str()));
    // And it survives the ping wire format unchanged.
    let (ping, _) = signed_ping("CORE", 1, "00").unwrap();
    let (pinger_id, _, _, _, _, _) = verify_signed_ping(&ping).unwrap();
    assert_eq!(pinger_id, first);
}

#[test]
fn peer_channel_ecdh_is_symmetric_and_seal_roundtrips() {
    let _t = set_key_env();
    let our_pub = node_pubkey_hex().expect("node pubkey");
    // The conversion invariant that makes seal/open agree across
    // two different nodes: the X25519 public derived from our
    // secret must equal the Montgomery map of our Ed25519 pubkey.
    let sk = node_x25519_secret().expect("x25519 secret");
    assert_eq!(
        x25519_dalek::PublicKey::from(&sk).as_bytes(),
        peer_x25519_public(&our_pub)
            .expect("peer x25519")
            .as_bytes()
    );
    let (nonce, ct) = member_seal(&our_pub, b"hello peer").expect("seal");
    assert_eq!(
        member_open(&our_pub, &nonce, &ct).expect("open"),
        b"hello peer"
    );
    // Tampered ciphertext and a wrong nonce both fail closed.
    let mut bad = ct.clone();
    bad[0] ^= 1;
    assert!(member_open(&our_pub, &nonce, &bad).is_none());
    assert!(member_open(&our_pub, &"00".repeat(12), &ct).is_none());
}

#[test]
fn v3_pong_carries_subject_attested_binding() {
    let _t = set_key_env();
    let (ping, nonce) = signed_ping_v2("CORE", 1, "00").expect("v2 ping");
    let (_, _, _, _, echoed, wants_v2) = verify_signed_ping(&ping).expect("verify");
    assert!(wants_v2);
    let my_id = wire_node_id();
    let my_pk = node_pubkey_hex().expect("pubkey");
    let pong = signed_pong_v3(&my_id, 3, "abcd", &echoed, &[]).expect("v3 pong");
    let (node_id, csum, _bloom, _roster, pubkey, bind_sig) =
        verify_signed_pong(&pong, &nonce).expect("v3 verify");
    assert_eq!(node_id, my_id);
    assert_eq!(csum, 3);
    assert_eq!(pubkey, my_pk);
    // The attestation must verify under the claimed key for the
    // claimed id — this is what member_add's subject_sig carries.
    assert!(verify_bind_attestation(&node_id, &pubkey, &bind_sig));
    // Wrong id, wrong key, and tampered sig all fail closed.
    assert!(!verify_bind_attestation(
        "susi-node-other",
        &pubkey,
        &bind_sig
    ));
    assert!(!verify_bind_attestation(
        &node_id,
        &"aa".repeat(32),
        &bind_sig
    ));
    let mut bad = bind_sig.clone();
    // Tamper deterministically — overwriting with a constant is a
    // no-op when the fresh signature already starts with that byte.
    bad.replace_range(
        0..2,
        if bind_sig.starts_with("00") {
            "11"
        } else {
            "00"
        },
    );
    assert!(!verify_bind_attestation(&node_id, &pubkey, &bad));
    // The real attack: a member holding cluster.key mints a
    // correctly-MAC'd v3 pong claiming a key the subject never
    // attested — the binding still refuses because the sig can't
    // verify under the claimed pubkey.
    let key = cluster_key().expect("cluster key");
    let fake_sig = "00".repeat(64);
    let mac = mac_tag(
        &key,
        &[
            "pong3", &my_id, &my_pk, &fake_sig, "3", "abcd", &echoed, "0",
        ],
    );
    let forged = format!("SUSI_PONG_SIG3:{my_id}:{my_pk}:{fake_sig}:3:abcd:{echoed}:0:{mac}");
    assert!(verify_signed_pong(&forged, &nonce).is_none());
    // And a v2 pong still verifies — with an empty attestation, so
    // member_add can't manufacture subject_sig from it.
    let pong2 = signed_pong_v2(&my_id, 3, "abcd", &echoed, &[]).expect("v2 pong");
    let (_, _, _, _, _, sig2) = verify_signed_pong(&pong2, &nonce).expect("v2 verify");
    assert!(sig2.is_empty());
}

#[test]
fn roster_codec_roundtrips_and_caps_entries() {
    assert_eq!(encode_roster(&[]), "0");
    assert!(decode_roster("0").is_empty());
    assert!(decode_roster("zz-not-hex").is_empty());
    let many: Vec<(String, String)> = (0..20)
        .map(|i| (format!("n{i}"), format!("10.0.0.{i}:9090")))
        .collect();
    let decoded = decode_roster(&encode_roster(&many));
    assert_eq!(decoded.len(), ROSTER_GOSSIP_MAX);
    assert_eq!(decoded[0], ("n0".to_string(), "10.0.0.0:9090".to_string()));
}
