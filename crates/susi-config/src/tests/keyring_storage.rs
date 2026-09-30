use crate::keyring_storage::{EncryptedFileFallback, FakeKeyring, KeyringBackend, KeyringStorage};

#[test]
fn keyring_storage_fake_roundtrip_and_env_override() {
    let mut store = KeyringStorage::new(FakeKeyring::default());
    store.put("openai", "sk-live").unwrap();
    assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-live"));
    store.set_env_override(
        &format!("{}/openai", KeyringStorage::<FakeKeyring>::service_name()),
        "sk-env",
    );
    assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-env"));
}

#[test]
fn keyring_storage_migrates_plaintext_cloud_env() {
    let mut store = KeyringStorage::new(FakeKeyring::default());
    let n = store
        .migrate_cloud_env("OPENAI_API_KEY=sk-a\nANTHROPIC_API_KEY=sk-b\n# comment\n")
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-a"));
    assert_eq!(store.get("anthropic").unwrap().as_deref(), Some("sk-b"));
}

#[test]
fn keyring_storage_encrypted_file_fallback_hides_plaintext() {
    let mut fb = EncryptedFileFallback::with_key(0x5A);
    fb.set("acct", "super-secret").unwrap();
    let stored = {
        // peek via get decode path
        fb.get("acct").unwrap().unwrap()
    };
    assert_eq!(stored, "super-secret");
    // raw map must not contain plaintext
    let raw = format!("{fb:?}");
    assert!(!raw.contains("super-secret"));
}
