use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::{bail, ensure};
use serde::{Deserialize, Serialize};

use crate::constants::GHOST_VERSION;

/// All fields extracted from a Ghost IPv6 address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostFields {
    pub platform_prefix: u32,
    pub version: u8,
    pub flags: u16,
    pub cluster_id: u32,
    pub vpc_id: u16,
    pub guest_ipv4: Ipv4Addr,
}

/// Construct a Ghost IPv6 address from its constituent fields.
///
/// Layout (128 bits):
/// ```text
/// b[0..4]   = platform_prefix (BE)
/// b[4]      = (version << 4) | flags_high_nibble
/// b[5]      = flags_low_byte
/// b[6..8]   = cluster_id bits 31..16  (high u16)
/// b[8..10]  = cluster_id bits 15..0   (low u16)
/// b[10..12] = vpc_id
/// b[12..16] = guest_ipv4
/// ```
pub fn construct(
    platform_prefix: u32,
    cluster_id: u32,
    vpc_id: u16,
    guest_ipv4: Ipv4Addr,
) -> Ipv6Addr {
    let mut b = [0u8; 16];

    // Platform prefix (32 bits)
    b[0..4].copy_from_slice(&platform_prefix.to_be_bytes());

    // Version (4 bits) + flags (12 bits, always 0)
    b[4] = GHOST_VERSION << 4; // ver=1, flags_high=0
    b[5] = 0x00; // flags_low=0

    // ClusterID (32 bits, split across two 16-bit groups)
    let cluster_hi = (cluster_id >> 16) as u16;
    let cluster_lo = (cluster_id & 0xFFFF) as u16;
    b[6..8].copy_from_slice(&cluster_hi.to_be_bytes());
    b[8..10].copy_from_slice(&cluster_lo.to_be_bytes());

    // VPC ID (16 bits)
    b[10..12].copy_from_slice(&vpc_id.to_be_bytes());

    // Guest IPv4 (32 bits)
    b[12..16].copy_from_slice(&guest_ipv4.octets());

    Ipv6Addr::from(b)
}

/// Parse a Ghost IPv6 address into its constituent fields.
pub fn parse(addr: Ipv6Addr) -> anyhow::Result<GhostFields> {
    let b = addr.octets();

    let platform_prefix = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let version = b[4] >> 4;
    let flags = (((b[4] & 0x0F) as u16) << 8) | (b[5] as u16);
    let cluster_hi = u16::from_be_bytes([b[6], b[7]]) as u32;
    let cluster_lo = u16::from_be_bytes([b[8], b[9]]) as u32;
    let cluster_id = (cluster_hi << 16) | cluster_lo;
    let vpc_id = u16::from_be_bytes([b[10], b[11]]);
    let guest_ipv4 = Ipv4Addr::new(b[12], b[13], b[14], b[15]);

    Ok(GhostFields {
        platform_prefix,
        version,
        flags,
        cluster_id,
        vpc_id,
        guest_ipv4,
    })
}

/// Validate that a Ghost IPv6 address has the expected prefix, version=1, and flags=0.
pub fn validate(addr: Ipv6Addr, expected_prefix: u32) -> anyhow::Result<()> {
    let fields = parse(addr)?;

    ensure!(
        fields.platform_prefix == expected_prefix,
        "prefix mismatch: expected {:#010x}, got {:#010x}",
        expected_prefix,
        fields.platform_prefix
    );

    if fields.version != GHOST_VERSION {
        bail!(
            "version mismatch: expected {}, got {}",
            GHOST_VERSION,
            fields.version
        );
    }

    if fields.flags != 0 {
        bail!("flags must be 0, got {:#05x}", fields.flags);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// Spec test vector: prefix=0xfd000001, cluster_id=1, vpc_id=5, ipv4=10.0.1.10
    /// → fd00:0001:1000:0000:0001:0005:0a00:010a
    #[test]
    fn test_spec_vector() {
        let addr = construct(0xfd00_0001, 1, 5, Ipv4Addr::new(10, 0, 1, 10));
        let expected: Ipv6Addr = "fd00:0001:1000:0000:0001:0005:0a00:010a".parse().unwrap();
        assert_eq!(addr, expected);
    }

    #[test]
    fn test_round_trip() {
        let prefix = 0xfd6b_3372u32;
        let cluster_id = 0x0001_0002u32;
        let vpc_id = 42u16;
        let ipv4 = Ipv4Addr::new(192, 168, 1, 100);

        let addr = construct(prefix, cluster_id, vpc_id, ipv4);
        let fields = parse(addr).unwrap();

        assert_eq!(fields.platform_prefix, prefix);
        assert_eq!(fields.version, GHOST_VERSION);
        assert_eq!(fields.flags, 0);
        assert_eq!(fields.cluster_id, cluster_id);
        assert_eq!(fields.vpc_id, vpc_id);
        assert_eq!(fields.guest_ipv4, ipv4);
    }

    #[test]
    fn test_validate_ok() {
        let prefix = 0xfd6b_3372u32;
        let addr = construct(prefix, 1, 1, Ipv4Addr::new(10, 0, 0, 1));
        validate(addr, prefix).unwrap();
    }

    #[test]
    fn test_validate_wrong_prefix() {
        let addr = construct(0xfd6b_3372, 1, 1, Ipv4Addr::new(10, 0, 0, 1));
        let err = validate(addr, 0xfd00_0001).unwrap_err();
        assert!(err.to_string().contains("prefix mismatch"));
    }

    #[test]
    fn test_validate_wrong_version() {
        // Manually craft an address with version=2
        let mut b = construct(0xfd6b_3372, 1, 1, Ipv4Addr::new(10, 0, 0, 1)).octets();
        b[4] = 0x20; // version=2, flags_high=0
        let addr = Ipv6Addr::from(b);
        let err = validate(addr, 0xfd6b_3372).unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    #[test]
    fn test_validate_nonzero_flags() {
        // Manually craft an address with flags != 0
        let mut b = construct(0xfd6b_3372, 1, 1, Ipv4Addr::new(10, 0, 0, 1)).octets();
        b[4] = 0x11; // version=1, flags_high_nibble=1
        let addr = Ipv6Addr::from(b);
        let err = validate(addr, 0xfd6b_3372).unwrap_err();
        assert!(err.to_string().contains("flags must be 0"));
    }

    #[test]
    fn test_zero_cluster_and_vpc() {
        let addr = construct(0xfd6b_3372, 0, 0, Ipv4Addr::new(0, 0, 0, 0));
        let fields = parse(addr).unwrap();
        assert_eq!(fields.cluster_id, 0);
        assert_eq!(fields.vpc_id, 0);
        assert_eq!(fields.guest_ipv4, Ipv4Addr::new(0, 0, 0, 0));
    }

    #[test]
    fn test_max_values() {
        let addr = construct(0xFFFF_FFFF, u32::MAX, u16::MAX, Ipv4Addr::new(255, 255, 255, 255));
        let fields = parse(addr).unwrap();
        assert_eq!(fields.platform_prefix, 0xFFFF_FFFF);
        assert_eq!(fields.cluster_id, u32::MAX);
        assert_eq!(fields.vpc_id, u16::MAX);
        assert_eq!(fields.guest_ipv4, Ipv4Addr::new(255, 255, 255, 255));
    }
}
