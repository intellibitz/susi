//! Canonical source mount for binaries that embed the ABI without a Cargo edge.

#[path = "cell.rs"]
pub mod cell;
#[path = "evidence.rs"]
pub mod evidence;
#[path = "memory.rs"]
pub mod memory;
#[path = "router.rs"]
pub mod router;
#[path = "swarm.rs"]
pub mod swarm;
#[path = "syscall.rs"]
pub mod syscall;
#[path = "wire.rs"]
pub mod wire;
