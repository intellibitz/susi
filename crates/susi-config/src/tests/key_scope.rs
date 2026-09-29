//! Tests for scoped key references (`zc_key_scope_*`).

use crate::key_scope::{resolve_key_ref, KeyRef, KeyScope};
use std::fs;
use std::path::PathBuf;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("susi_zc_key_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn zc_key_scope_parse_uri_and_bare_env() {
    let r = KeyRef::parse("key://env/OPENAI_API_KEY").unwrap();
    assert_eq!(r.scope, KeyScope::Env);
    assert_eq!(r.name, "OPENAI_API_KEY");
    assert_eq!(r.uri(), "key://env/OPENAI_API_KEY");

    let bare = KeyRef::parse("HF_TOKEN").unwrap();
    assert_eq!(bare.scope, KeyScope::Env);
    assert_eq!(bare.name, "HF_TOKEN");

    let cloud = KeyRef::parse("key://cloud/ANTHROPIC_API_KEY").unwrap();
    assert_eq!(cloud.scope, KeyScope::CloudEnv);

    let host = KeyRef::parse("key://host/api_token").unwrap();
    assert_eq!(host.scope, KeyScope::HostFile);

    assert!(KeyRef::parse("key://bad/x").is_err());
    assert!(KeyRef::parse("key://env/").is_err());
}

#[test]
fn zc_key_scope_resolves_host_file_without_copying_secret() {
    let dir = scratch("host");
    fs::write(dir.join("api_token"), "tok-live-value\n").unwrap();
    let reference = KeyRef::parse("key://host/api_token").unwrap();
    let secret = resolve_key_ref(&reference, &dir).unwrap();
    assert_eq!(secret.value, "tok-live-value");
    assert!(secret.render().contains("[redacted]"));
    assert!(!secret.render().contains("tok-live-value"));
    // Secret must not have been written into any sibling file.
    let siblings: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(siblings, vec!["api_token".to_string()]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zc_key_scope_resolves_cloud_env_alias() {
    let dir = scratch("cloud");
    fs::write(
        dir.join("cloud.env"),
        "OPENAI_API_KEY=sk-from-cloud-env\n# comment\n",
    )
    .unwrap();
    let reference = KeyRef::parse("key://cloud/OPENAI_API_KEY").unwrap();
    let secret = resolve_key_ref(&reference, &dir).unwrap();
    assert_eq!(secret.value, "sk-from-cloud-env");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zc_key_scope_env_process_wins() {
    let _lock = crate::env_test_lock();
    let dir = scratch("env");
    unsafe {
        std::env::set_var("SUSI_ZC_KEY_SCOPE_TEST", "from-process");
    }
    let reference = KeyRef::parse("key://env/SUSI_ZC_KEY_SCOPE_TEST").unwrap();
    let secret = resolve_key_ref(&reference, &dir).unwrap();
    assert_eq!(secret.value, "from-process");
    unsafe {
        std::env::remove_var("SUSI_ZC_KEY_SCOPE_TEST");
    }
    let _ = fs::remove_dir_all(&dir);
}
