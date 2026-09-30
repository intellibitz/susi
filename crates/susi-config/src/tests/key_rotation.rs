use crate::key_rotation::{
    format_keys_list, key_statuses, quarantine_path_in, rotate_key, KeyAges, RotateKey,
};
use std::env::temp_dir;
use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let d = temp_dir().join(format!("keyrot-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn key_rotation_ages_track_set_time() {
    let mut ages = KeyAges::default();
    ages.stamp("OPENAI_API_KEY", 1_000_000);
    assert_eq!(
        ages.age_days("OPENAI_API_KEY", 1_000_000 + 2 * 86_400),
        Some(2)
    );
    assert_eq!(ages.age_days("MISSING", 1_000_000), None);
}

#[test]
fn key_rotation_list_flags_old_keys() {
    let env = "OPENAI_API_KEY=sk-1\nHF_TOKEN=hf-1\n";
    let mut ages = KeyAges::default();
    ages.stamp("OPENAI_API_KEY", 1_000_000);
    ages.stamp("HF_TOKEN", 100_000_000);
    let now = 100_000_000;
    let st = key_statuses(env, &ages, now, 90);
    assert_eq!(st.len(), 2);
    assert!(st[0].needs_rotation, "100+ day old key must flag");
    assert!(!st[1].needs_rotation);
    let report = format_keys_list(&st);
    assert!(report.contains("ROTATE"));
    assert!(
        !report.contains("sk-1"),
        "report must never leak the secret"
    );
}

#[test]
fn key_rotation_unknown_age_not_flagged() {
    let env = "FRESH_KEY=x\n";
    let st = key_statuses(env, &KeyAges::default(), 0, 90);
    assert_eq!(st[0].age_days, None);
    assert!(!st[0].needs_rotation);
}

#[test]
fn key_rotation_swaps_atomically_and_clears_quarantine() {
    let dir = tmpdir("swap");
    let env_path = dir.join("cloud.env");
    let ages_path = KeyAges::path_in(&dir);
    std::fs::write(
        &env_path,
        "# comment\nOPENAI_API_KEY=sk-old\nHF_TOKEN=hf-keep\n",
    )
    .unwrap();
    let qdir = dir.join("key-quarantine");
    std::fs::create_dir_all(&qdir).unwrap();
    std::fs::write(quarantine_path_in(&dir, "openai"), "401s").unwrap();

    rotate_key(&RotateKey {
        env_path: &env_path,
        ages_path: &ages_path,
        config_dir: &dir,
        key: "OPENAI_API_KEY",
        vendor: "openai",
        new_value: "sk-new",
        now_unix: 5_000,
    })
    .unwrap();

    let after = std::fs::read_to_string(&env_path).unwrap();
    assert!(after.contains("OPENAI_API_KEY=sk-new"));
    assert!(after.contains("HF_TOKEN=hf-keep"), "other keys untouched");
    assert!(after.contains("# comment"), "comments preserved");
    assert!(
        !quarantine_path_in(&dir, "openai").exists(),
        "quarantine cleared"
    );
    assert_eq!(
        KeyAges::load(&ages_path).unwrap().set_unix["OPENAI_API_KEY"],
        5_000
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn key_rotation_refuses_unknown_key() {
    let dir = tmpdir("unknown");
    let env_path = dir.join("cloud.env");
    std::fs::write(&env_path, "A=1\n").unwrap();
    let r = rotate_key(&RotateKey {
        env_path: &env_path,
        ages_path: &KeyAges::path_in(&dir),
        config_dir: &dir,
        key: "NOPE",
        vendor: "v",
        new_value: "x",
        now_unix: 0,
    });
    assert!(r.is_err());
    assert_eq!(std::fs::read_to_string(&env_path).unwrap(), "A=1\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn key_rotation_ages_persist_roundtrip() {
    let dir = tmpdir("persist");
    let path = KeyAges::path_in(&dir);
    let mut ages = KeyAges::default();
    ages.stamp("K", 42);
    ages.save(&path).unwrap();
    let loaded = KeyAges::load(&path).unwrap();
    assert_eq!(loaded.set_unix["K"], 42);
    assert!(KeyAges::load(&dir.join("absent.json"))
        .unwrap()
        .set_unix
        .is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
