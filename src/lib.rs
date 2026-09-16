pub mod daemon;
pub mod error;
pub mod gawd;
pub mod gemi;
pub mod gmcp;
pub mod native;
pub mod sandbox;

pub const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");
