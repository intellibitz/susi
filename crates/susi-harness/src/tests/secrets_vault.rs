use super::*;

#[test]
fn sealed_vault_supplies_harness_keys_without_dotenv_or_process_env() {
    let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let root = std::env::temp_dir().join(format!("susi_harness_sealed_{}", std::process::id()));
    let dir = root.join(".susi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let restore = |k: &str, v: Option<std::ffi::OsString>| match v {
        Some(val) => std::env::set_var(k, val),
        None => std::env::remove_var(k),
    };
    let key_env = susi_config::vault_store::VAULT_KEY_ENV;
    let prev = [
        ("SUSI_HOME", std::env::var_os("SUSI_HOME")),
        (
            "SUSI_NO_REGISTRY_ENV",
            std::env::var_os("SUSI_NO_REGISTRY_ENV"),
        ),
        (key_env, std::env::var_os(key_env)),
    ];
    std::env::set_var("SUSI_HOME", &dir);
    std::env::remove_var("SUSI_NO_REGISTRY_ENV");
    std::env::remove_var(key_env);

    const KEY: &str = "SUSI_TEST_SEALED_HARNESS_0001";
    susi_config::vault_store::enroll_master_key(&dir, Some("keyfile"), None).unwrap();
    let mut vault = susi_config::vault_store::SealedVault::open_or_init(&dir).unwrap();
    vault.set("cloud", KEY, "sk-harness-sealed-0001");
    vault.save(&dir).unwrap();
    assert_eq!(lookup(KEY).unwrap().expose(), "sk-harness-sealed-0001");

    // Key rotation in sealed vault and process-env overrides must be observed immediately,
    // while prior keys remain registered for redaction.
    vault.set("cloud", KEY, "sk-harness-sealed-rotated-0002");
    vault.save(&dir).unwrap();
    assert_eq!(
        lookup(KEY).unwrap().expose(),
        "sk-harness-sealed-rotated-0002"
    );
    assert_eq!(redact("leaked sk-harness-sealed-0001"), "leaked <redacted>");

    std::env::set_var(KEY, "sk-process-override-0003");
    assert_eq!(lookup(KEY).unwrap().expose(), "sk-process-override-0003");
    std::env::remove_var(KEY);
    assert_eq!(
        lookup(KEY).unwrap().expose(),
        "sk-harness-sealed-rotated-0002"
    );

    for (k, v) in prev {
        restore(k, v);
    }
    let _ = std::fs::remove_dir_all(&root);
}
