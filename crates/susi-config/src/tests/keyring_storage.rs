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

#[test]
fn keyring_storage_delete_and_missing_account_lookup_return_expected_results() {
    let mut store = KeyringStorage::new(FakeKeyring::default());
    assert_eq!(store.get("nonexistent").unwrap(), None);

    store.put("openai", "sk-1").unwrap();
    assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-1"));

    let account_key = format!("{}/openai", KeyringStorage::<FakeKeyring>::service_name());
    store.backend_mut().delete(&account_key).unwrap();
    assert_eq!(store.get("openai").unwrap(), None);

    let mut fb = EncryptedFileFallback::with_key(0x33);
    fb.set("acct", "secret-val").unwrap();
    assert_eq!(fb.get("acct").unwrap().as_deref(), Some("secret-val"));
    fb.delete("acct").unwrap();
    assert_eq!(fb.get("acct").unwrap(), None);
}

#[test]
fn keyring_storage_service_name_is_non_empty_and_overwrite_updates_value() {
    let svc = KeyringStorage::<FakeKeyring>::service_name();
    assert!(!svc.is_empty());
    let mut store = KeyringStorage::new(FakeKeyring::default());
    store.put("gemini", "first-secret").unwrap();
    store.put("gemini", "second-secret").unwrap();
    assert_eq!(
        store.get("gemini").unwrap().as_deref(),
        Some("second-secret")
    );
}
