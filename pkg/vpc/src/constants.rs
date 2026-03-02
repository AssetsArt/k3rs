/// ULA prefix encoding "k3rs": fd + 6b337273 → 0xfd6b3372 (truncated to 32 bits).
pub const PLATFORM_PREFIX: u32 = 0xfd6b_3372;

/// Ghost IPv6 address version field (must be 1).
pub const GHOST_VERSION: u8 = 1;

/// VPC ID 0 is reserved (never assigned to user VPCs).
pub const RESERVED_VPC_ID: u16 = 0;

/// The default VPC created at cluster init.
pub const DEFAULT_VPC_ID: u16 = 1;

/// State store key where the cluster's unique ID is persisted.
pub const CLUSTER_ID_KEY: &str = "/registry/cluster/id";
