pub mod manager;
pub mod xdg {
    pub use susi_paths::SusiDirs;
}
pub use manager::SandboxManager;
pub mod versioned_store;
pub use versioned_store::VersionedJsonStore;
pub mod auto_install;
