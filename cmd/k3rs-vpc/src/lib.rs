//! Library target for k3rs-vpc — re-exports modules needed by integration tests
//! and shared between the library and binary targets.

pub mod enforcer;
pub mod noop_enforcer;
pub mod protocol;
pub mod store;

// The following modules are only used by the binary (main.rs):
// allocator, nftables, socket, sync, ebpf_enforcer
