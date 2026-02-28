//! State store / leader election constants.

/// etcd-style key for the controller leader lease.
pub const LEADER_LEASE_KEY: &str = "/registry/leases/controller-leader";

/// How long a leader lease is valid, in seconds.
pub const LEADER_LEASE_TTL_SECS: u64 = 15;

/// The lease is renewed every `TTL / LEADER_RENEW_INTERVAL_DIVISOR` seconds.
pub const LEADER_RENEW_INTERVAL_DIVISOR: u64 = 3;
