//! Comprehensive verification test suite for SUSI Bring Your Own Key (BYOK) capabilities.

use std::time::{SystemTime, UNIX_EPOCH};
use susi_vendor_models::cloud_credentials;

fn unique_test_dir(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("susi-byok-test-{name}-{nanos}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[test]
fn test_byok_credential_lifecycle_and_spend_caps() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = unique_test_dir("lifecycle")?;
    let store_path = &temp_dir;

    // 1. Save credentials with budgets and labels
    let cred_id1 = cloud_credentials::save_with_options(
        store_path,
        "ANTHROPIC_API_KEY",
        "sk-ant-api03-testkey-12345678",
        Some(25.00),
        Some("primary-anthropic-key".to_string()),
    )?;

    let cred_id2 = cloud_credentials::save_with_options(
        store_path,
        "OPENAI_API_KEY",
        "sk-proj-testkey-87654321",
        Some(10.00),
        Some("backup-openai-key".to_string()),
    )?;

    assert_ne!(cred_id1, cred_id2);

    // 2. Verify listing credentials
    let all_creds = cloud_credentials::list_all(store_path)?;
    assert_eq!(all_creds.len(), 2);

    let anthropic_creds = cloud_credentials::list(store_path, "ANTHROPIC_API_KEY")?;
    assert_eq!(anthropic_creds.len(), 1);
    assert_eq!(
        anthropic_creds[0].label.as_deref(),
        Some("primary-anthropic-key")
    );
    assert_eq!(anthropic_creds[0].mask_key(), "sk-a...5678");
    assert!(!anthropic_creds[0].is_budget_exceeded());

    // 3. Test spend recording and budget cap enforcement
    cloud_credentials::record_spend(store_path, &cred_id1, 15.50)?;

    let cred1_updated =
        cloud_credentials::get_by_id(store_path, &cred_id1)?.ok_or("credential missing")?;
    assert_eq!(cred1_updated.current_spend_usd, Some(15.50));
    assert!(!cred1_updated.is_budget_exceeded());

    // Exceed budget cap
    cloud_credentials::record_spend(store_path, &cred_id1, 10.50)?;

    let cred1_exceeded =
        cloud_credentials::get_by_id(store_path, &cred_id1)?.ok_or("credential missing")?;
    assert_eq!(cred1_exceeded.current_spend_usd, Some(26.00));
    assert!(cred1_exceeded.is_budget_exceeded());

    // 4. Test credential removal by ID
    cloud_credentials::remove_by_id(store_path, &cred_id2)?;
    let remaining = cloud_credentials::list_all(store_path)?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, cred_id1);

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[test]
fn test_byok_credential_masking_security() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = unique_test_dir("masking")?;
    let store_path = &temp_dir;

    let cred_id = cloud_credentials::save(
        store_path,
        "DEEPSEEK_API_KEY",
        "sk-dsh-secret-key-999988887777",
    )?;

    let cred = cloud_credentials::get_by_id(store_path, &cred_id)?.ok_or("missing credential")?;

    // Ensure raw secret string is masked
    let masked = cred.mask_key();
    assert!(!masked.contains("secret"));
    assert_eq!(masked, "sk-d...7777");

    // Ensure debug output does not leak raw secret key
    let debug_str = format!("{cred:?}");
    assert!(!debug_str.contains("sk-dsh-secret-key"));
    assert!(debug_str.contains("masked_key"));

    let _ = std::fs::remove_dir_all(&temp_dir);
    Ok(())
}
