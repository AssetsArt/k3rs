//! Minimal DHCP client for obtaining network configuration.
//!
//! Performs a standard DHCP exchange (DISCOVER → OFFER → REQUEST → ACK)
//! using raw UDP sockets. Designed for the minimal k3rs-init environment
//! where no userspace DHCP client is available.
//!
//! Used by Apple's Virtualization.framework which provides a built-in
//! DHCP server on its NAT network (typically 192.168.64.0/24).

#[cfg(target_os = "linux")]
use std::io::Write;

/// Result of a successful DHCP exchange.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DhcpLease {
    /// Assigned IP address (e.g. "192.168.64.2")
    pub ip: String,
    /// Subnet mask as prefix length (e.g. 24)
    pub prefix_len: u32,
    /// Default gateway IP (e.g. "192.168.64.1")
    pub gateway: Option<String>,
    /// DNS server IPs
    pub dns_servers: Vec<String>,
}

// ─── DHCP Constants ──────────────────────────────────────────────

#[cfg(target_os = "linux")]
const DHCP_SERVER_PORT: u16 = 67;
#[cfg(target_os = "linux")]
const DHCP_CLIENT_PORT: u16 = 68;

// DHCP message types
#[cfg(target_os = "linux")]
const DHCP_DISCOVER: u8 = 1;
#[cfg(target_os = "linux")]
const DHCP_OFFER: u8 = 2;
#[cfg(target_os = "linux")]
const DHCP_REQUEST: u8 = 3;
#[cfg(target_os = "linux")]
const DHCP_ACK: u8 = 5;

// DHCP options
#[cfg(target_os = "linux")]
const OPT_SUBNET_MASK: u8 = 1;
#[cfg(target_os = "linux")]
const OPT_ROUTER: u8 = 3;
#[cfg(target_os = "linux")]
const OPT_DNS: u8 = 6;
#[cfg(target_os = "linux")]
const OPT_REQUESTED_IP: u8 = 50;
#[cfg(target_os = "linux")]
const OPT_MSG_TYPE: u8 = 53;
#[cfg(target_os = "linux")]
const OPT_SERVER_ID: u8 = 54;
#[cfg(target_os = "linux")]
const OPT_PARAM_LIST: u8 = 55;
#[cfg(target_os = "linux")]
const OPT_END: u8 = 255;

// DHCP magic cookie
#[cfg(target_os = "linux")]
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

// Transaction ID (static, we only do one exchange at a time)
#[cfg(target_os = "linux")]
const XID: u32 = 0x4b335253; // "K3RS" in hex

// ─── Public API ──────────────────────────────────────────────────

/// Perform DHCP on the given interface. Returns a lease on success.
///
/// The interface must already be UP (link layer active).
/// Retries the DISCOVER up to 3 times with exponential backoff.
#[cfg(target_os = "linux")]
pub fn do_dhcp(iface: &str) -> Result<DhcpLease, Box<dyn std::error::Error>> {
    // Create raw UDP socket bound to 0.0.0.0:68
    let sock = create_dhcp_socket(iface)?;

    // Try up to 3 times with increasing timeout
    for attempt in 0..3 {
        let timeout_ms = 2000 * (1 << attempt); // 2s, 4s, 8s

        // 1. DISCOVER
        log_info!(
            "DHCP DISCOVER on {} (attempt {}, timeout {}ms)",
            iface,
            attempt + 1,
            timeout_ms
        );
        let discover = build_discover();
        send_broadcast(sock, &discover)?;

        // 2. Wait for OFFER
        set_recv_timeout(sock, timeout_ms)?;
        let offer = match recv_dhcp(sock) {
            Ok(msg) => msg,
            Err(_) => {
                log_info!("DHCP: no OFFER received, retrying...");
                continue;
            }
        };

        let offer_type = get_msg_type(&offer);
        if offer_type != Some(DHCP_OFFER) {
            log_info!("DHCP: expected OFFER, got type {:?}", offer_type);
            continue;
        }

        let offered_ip = format!("{}.{}.{}.{}", offer[16], offer[17], offer[18], offer[19]);
        let server_ip = get_option_ip(&offer, OPT_SERVER_ID);

        log_info!("DHCP OFFER: ip={} server={:?}", offered_ip, server_ip);

        // 3. REQUEST
        let request = build_request(&offer, &offered_ip, server_ip.as_deref());
        send_broadcast(sock, &request)?;

        // 4. Wait for ACK
        let ack = match recv_dhcp(sock) {
            Ok(msg) => msg,
            Err(_) => {
                log_info!("DHCP: no ACK received, retrying...");
                continue;
            }
        };

        let ack_type = get_msg_type(&ack);
        if ack_type != Some(DHCP_ACK) {
            log_info!("DHCP: expected ACK, got type {:?}", ack_type);
            continue;
        }

        // Parse lease from ACK
        let lease = parse_lease(&ack);
        log_info!(
            "DHCP ACK: ip={} prefix={} gw={:?} dns={:?}",
            lease.ip,
            lease.prefix_len,
            lease.gateway,
            lease.dns_servers
        );

        unsafe { libc::close(sock) };
        return Ok(lease);
    }

    unsafe { libc::close(sock) };
    Err("DHCP: failed after 3 attempts".into())
}

// ─── Socket helpers ──────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn create_dhcp_socket(iface: &str) -> Result<i32, Box<dyn std::error::Error>> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_UDP) };
    if sock < 0 {
        return Err(format!("socket() failed: {}", std::io::Error::last_os_error()).into());
    }

    // Allow broadcast
    let opt: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_BROADCAST,
            &opt as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    // Reuse address
    unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &opt as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    // Bind to specific interface
    let iface_bytes = iface.as_bytes();
    let mut ifname = [0u8; libc::IFNAMSIZ];
    let copy_len = iface_bytes.len().min(libc::IFNAMSIZ - 1);
    ifname[..copy_len].copy_from_slice(&iface_bytes[..copy_len]);
    unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            ifname.as_ptr() as *const libc::c_void,
            (copy_len + 1) as libc::socklen_t,
        );
    }

    // Bind to 0.0.0.0:68
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = DHCP_CLIENT_PORT.to_be();
    addr.sin_addr.s_addr = 0; // INADDR_ANY

    let ret = unsafe {
        libc::bind(
            sock,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        unsafe { libc::close(sock) };
        return Err(format!("bind() failed: {}", std::io::Error::last_os_error()).into());
    }

    Ok(sock)
}

#[cfg(target_os = "linux")]
fn set_recv_timeout(sock: i32, ms: u64) -> Result<(), Box<dyn std::error::Error>> {
    let tv = libc::timeval {
        tv_sec: (ms / 1000) as libc::time_t,
        tv_usec: ((ms % 1000) * 1000) as libc::suseconds_t,
    };
    let ret = unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(format!(
            "setsockopt(SO_RCVTIMEO) failed: {}",
            std::io::Error::last_os_error()
        )
        .into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn send_broadcast(sock: i32, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut dest: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    dest.sin_family = libc::AF_INET as libc::sa_family_t;
    dest.sin_port = DHCP_SERVER_PORT.to_be();
    dest.sin_addr.s_addr = u32::MAX; // 255.255.255.255

    let ret = unsafe {
        libc::sendto(
            sock,
            data.as_ptr() as *const libc::c_void,
            data.len(),
            0,
            &dest as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(format!("sendto() failed: {}", std::io::Error::last_os_error()).into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn recv_dhcp(sock: i32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut buf = vec![0u8; 1500];
    let n = unsafe {
        libc::recvfrom(
            sock,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if n < 0 {
        return Err(format!("recvfrom() failed: {}", std::io::Error::last_os_error()).into());
    }
    buf.truncate(n as usize);

    // Validate: must be a DHCP reply (op=2) with our XID
    if buf.len() < 240 {
        return Err("DHCP: packet too short".into());
    }
    if buf[0] != 2 {
        return Err("DHCP: not a reply".into());
    }
    let rxid = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if rxid != XID {
        return Err(format!("DHCP: XID mismatch: expected {:08x}, got {:08x}", XID, rxid).into());
    }

    Ok(buf)
}

// ─── Packet builders ─────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn build_discover() -> Vec<u8> {
    let mut pkt = vec![0u8; 300];

    pkt[0] = 1; // op: BOOTREQUEST
    pkt[1] = 1; // htype: Ethernet
    pkt[2] = 6; // hlen: MAC length
    pkt[3] = 0; // hops

    // xid
    let xid = XID.to_be_bytes();
    pkt[4..8].copy_from_slice(&xid);

    // flags: broadcast
    pkt[10] = 0x80;
    pkt[11] = 0x00;

    // chaddr: use a static MAC (will be replaced by actual MAC from eth0)
    // We use a k3rs-specific MAC prefix
    pkt[28] = 0x02;
    pkt[29] = 0x6b;
    pkt[30] = 0x33;
    pkt[31] = 0x72;
    pkt[32] = 0x73;
    pkt[33] = 0x01;

    // Try to read actual MAC from sysfs
    if let Ok(mac_str) = std::fs::read_to_string("/sys/class/net/eth0/address") {
        let parts: Vec<u8> = mac_str
            .trim()
            .split(':')
            .filter_map(|s| u8::from_str_radix(s, 16).ok())
            .collect();
        if parts.len() == 6 {
            pkt[28..34].copy_from_slice(&parts);
        }
    }

    // Magic cookie
    pkt[236..240].copy_from_slice(&MAGIC_COOKIE);

    // Options
    let mut pos = 240;

    // Option 53: DHCP Message Type = DISCOVER
    pkt[pos] = OPT_MSG_TYPE;
    pkt[pos + 1] = 1;
    pkt[pos + 2] = DHCP_DISCOVER;
    pos += 3;

    // Option 55: Parameter Request List
    pkt[pos] = OPT_PARAM_LIST;
    pkt[pos + 1] = 3;
    pkt[pos + 2] = OPT_SUBNET_MASK;
    pkt[pos + 3] = OPT_ROUTER;
    pkt[pos + 4] = OPT_DNS;
    pos += 5;

    // End
    pkt[pos] = OPT_END;

    pkt.truncate(pos + 1);
    pkt
}

#[cfg(target_os = "linux")]
fn build_request(offer: &[u8], offered_ip: &str, server_ip: Option<&str>) -> Vec<u8> {
    let mut pkt = vec![0u8; 300];

    pkt[0] = 1; // op: BOOTREQUEST
    pkt[1] = 1; // htype: Ethernet
    pkt[2] = 6; // hlen
    pkt[3] = 0; // hops

    // Copy xid from offer
    pkt[4..8].copy_from_slice(&offer[4..8]);

    // flags: broadcast
    pkt[10] = 0x80;

    // Copy chaddr from offer
    pkt[28..44].copy_from_slice(&offer[28..44]);

    // Magic cookie
    pkt[236..240].copy_from_slice(&MAGIC_COOKIE);

    let mut pos = 240;

    // Option 53: DHCP Message Type = REQUEST
    pkt[pos] = OPT_MSG_TYPE;
    pkt[pos + 1] = 1;
    pkt[pos + 2] = DHCP_REQUEST;
    pos += 3;

    // Option 50: Requested IP
    if let Ok(ip_bytes) = parse_ip_to_bytes(offered_ip) {
        pkt[pos] = OPT_REQUESTED_IP;
        pkt[pos + 1] = 4;
        pkt[pos + 2..pos + 6].copy_from_slice(&ip_bytes);
        pos += 6;
    }

    // Option 54: Server Identifier
    if let Some(srv) = server_ip {
        if let Ok(srv_bytes) = parse_ip_to_bytes(srv) {
            pkt[pos] = OPT_SERVER_ID;
            pkt[pos + 1] = 4;
            pkt[pos + 2..pos + 6].copy_from_slice(&srv_bytes);
            pos += 6;
        }
    }

    // Option 55: Parameter Request List
    pkt[pos] = OPT_PARAM_LIST;
    pkt[pos + 1] = 3;
    pkt[pos + 2] = OPT_SUBNET_MASK;
    pkt[pos + 3] = OPT_ROUTER;
    pkt[pos + 4] = OPT_DNS;
    pos += 5;

    // End
    pkt[pos] = OPT_END;

    pkt.truncate(pos + 1);
    pkt
}

// ─── Option parsers ──────────────────────────────────────────────

/// Get the DHCP message type option (53) from a DHCP packet.
#[cfg(target_os = "linux")]
fn get_msg_type(pkt: &[u8]) -> Option<u8> {
    for_each_option(pkt, |opt, data| {
        if opt == OPT_MSG_TYPE && !data.is_empty() {
            return Some(data[0]);
        }
        None
    })
}

/// Get an IP from a specific option (4 bytes → dotted quad).
#[cfg(target_os = "linux")]
fn get_option_ip(pkt: &[u8], opt_code: u8) -> Option<String> {
    for_each_option(pkt, |opt, data| {
        if opt == opt_code && data.len() >= 4 {
            return Some(format!("{}.{}.{}.{}", data[0], data[1], data[2], data[3]));
        }
        None
    })
}

/// Get all IPs from a specific option (may contain multiple 4-byte IPs).
#[cfg(target_os = "linux")]
fn get_option_ips(pkt: &[u8], opt_code: u8) -> Vec<String> {
    let mut result = Vec::new();
    for_each_option::<()>(pkt, |opt, data| {
        if opt == opt_code {
            let mut i = 0;
            while i + 4 <= data.len() {
                result.push(format!(
                    "{}.{}.{}.{}",
                    data[i],
                    data[i + 1],
                    data[i + 2],
                    data[i + 3]
                ));
                i += 4;
            }
        }
        None
    });
    result
}

/// Iterate over DHCP options, calling `f` for each. Returns early if `f` returns `Some`.
#[cfg(target_os = "linux")]
fn for_each_option<T>(pkt: &[u8], mut f: impl FnMut(u8, &[u8]) -> Option<T>) -> Option<T> {
    if pkt.len() < 240 {
        return None;
    }
    // Verify magic cookie
    if pkt[236..240] != MAGIC_COOKIE {
        return None;
    }

    let mut pos = 240;
    while pos < pkt.len() {
        let opt = pkt[pos];
        if opt == OPT_END {
            break;
        }
        if opt == 0 {
            // Padding
            pos += 1;
            continue;
        }
        if pos + 1 >= pkt.len() {
            break;
        }
        let len = pkt[pos + 1] as usize;
        if pos + 2 + len > pkt.len() {
            break;
        }
        let data = &pkt[pos + 2..pos + 2 + len];
        if let Some(result) = f(opt, data) {
            return Some(result);
        }
        pos += 2 + len;
    }
    None
}

/// Parse a DHCP ACK into a DhcpLease.
#[cfg(target_os = "linux")]
fn parse_lease(ack: &[u8]) -> DhcpLease {
    let ip = format!("{}.{}.{}.{}", ack[16], ack[17], ack[18], ack[19]);

    // Subnet mask → prefix length
    let prefix_len = if let Some(mask_str) = get_option_ip(ack, OPT_SUBNET_MASK) {
        mask_to_prefix(&mask_str)
    } else {
        24 // Default
    };

    let gateway = get_option_ip(ack, OPT_ROUTER);
    let dns_servers = get_option_ips(ack, OPT_DNS);

    DhcpLease {
        ip,
        prefix_len,
        gateway,
        dns_servers,
    }
}

/// Convert dotted-quad subnet mask to prefix length (e.g. "255.255.255.0" → 24).
#[cfg(target_os = "linux")]
fn mask_to_prefix(mask: &str) -> u32 {
    let parts: Vec<u8> = mask.split('.').filter_map(|s| s.parse().ok()).collect();
    if parts.len() != 4 {
        return 24;
    }
    let bits = u32::from_be_bytes([parts[0], parts[1], parts[2], parts[3]]);
    bits.leading_ones()
}

/// Parse dotted-quad IP to 4-byte array.
#[cfg(target_os = "linux")]
fn parse_ip_to_bytes(ip: &str) -> Result<[u8; 4], Box<dyn std::error::Error>> {
    let parts: Vec<u8> = ip
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() != 4 {
        return Err(format!("invalid IP: {}", ip).into());
    }
    Ok([parts[0], parts[1], parts[2], parts[3]])
}
