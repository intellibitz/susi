//! Test probe for [`susi_paths::test_env::isolate_home`].
//!
//! Prints the directories [`susi_paths::SusiDirs`] resolves in this process,
//! one `key=path` line each, and the port offset it would run at.
//! `tests/isolate_home.rs` launches it through the helper and asserts every
//! dir sits under the home it was given: the child resolves its dirs exactly
//! as the `susi` binary does, from the env it was handed, so this is what a
//! test's `susi` child would read and write.
fn main() {
    use susi_paths::SusiDirs;
    for (key, dir) in [
        ("home_dir", SusiDirs::home_dir()),
        ("config_dir", SusiDirs::config_dir()),
        ("data_dir", SusiDirs::data_dir()),
        ("cache_dir", SusiDirs::cache_dir()),
        ("substrate_home", SusiDirs::substrate_home()),
    ] {
        println!("{key}={}", dir.display());
    }
    println!("port_offset={}", susi_paths::ports::env_port_offset());
}
