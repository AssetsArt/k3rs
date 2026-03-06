use std::io::Write;
use std::path::Path;

/// k3rs VPC boot parameters parsed from /proc/cmdline.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct K3rsBootParams {
    pub ipv4: Option<String>,         // k3rs.ipv4=10.0.1.5
    pub ipv6: Option<String>,         // k3rs.ipv6=fd6b:3372:...
    pub vpc_id: Option<u16>,          // k3rs.vpc_id=42
    pub vpc_cidr: Option<String>,     // k3rs.vpc_cidr=10.0.1.0/24
    pub gw_mac: Option<String>,       // k3rs.gw_mac=02:fc:00:00:00:01
    pub platform_prefix: Option<u32>, // k3rs.platform_prefix=0xfd6b3372
    pub cluster_id: Option<u32>,      // k3rs.cluster_id=1
}

#[cfg(target_os = "linux")]
impl K3rsBootParams {
    /// Returns true if all required VPC parameters are present.
    pub fn is_complete(&self) -> bool {
        self.ipv4.is_some()
            && self.ipv6.is_some()
            && self.vpc_id.is_some()
            && self.vpc_cidr.is_some()
            && self.gw_mac.is_some()
            && self.platform_prefix.is_some()
            && self.cluster_id.is_some()
    }
}

/// Parse k3rs-specific boot parameters from /proc/cmdline.
#[cfg(target_os = "linux")]
pub fn parse_cmdline() -> K3rsBootParams {
    let mut params = K3rsBootParams::default();

    let cmdline = match std::fs::read_to_string("/proc/cmdline") {
        Ok(s) => s,
        Err(_) => return params,
    };

    for token in cmdline.split_whitespace() {
        if let Some((key, val)) = token.split_once('=') {
            match key {
                "k3rs.ipv4" => params.ipv4 = Some(val.to_string()),
                "k3rs.ipv6" => params.ipv6 = Some(val.to_string()),
                "k3rs.vpc_id" => params.vpc_id = val.parse().ok(),
                "k3rs.vpc_cidr" => params.vpc_cidr = Some(val.to_string()),
                "k3rs.gw_mac" => params.gw_mac = Some(val.to_string()),
                "k3rs.platform_prefix" => {
                    params.platform_prefix =
                        u32::from_str_radix(val.trim_start_matches("0x"), 16).ok();
                }
                "k3rs.cluster_id" => params.cluster_id = val.parse().ok(),
                _ => {}
            }
        }
    }

    params
}

/// Setup networking: bring up loopback and eth0, optionally configure VPC networking.
#[cfg(target_os = "linux")]
pub fn setup_networking() -> Result<(), Box<dyn std::error::Error>> {
    // Bring up loopback
    if Path::new("/sys/class/net/lo/operstate").exists() {
        bring_interface_up("lo")?;
        log_info!("loopback interface up");
    }

    // Bring up eth0 (virtio-net)
    if Path::new("/sys/class/net/eth0/operstate").exists() {
        bring_interface_up("eth0")?;
        log_info!("eth0 interface up");
    }

    // Parse VPC boot parameters
    let params = parse_cmdline();
    if params.is_complete() {
        let ipv4 = params.ipv4.as_ref().unwrap();
        let ipv6 = params.ipv6.as_ref().unwrap();
        let vpc_cidr = params.vpc_cidr.as_ref().unwrap();
        let gw_mac = params.gw_mac.as_ref().unwrap();

        log_info!(
            "VPC params: ipv4={} ipv6={} vpc_id={} cidr={} gw_mac={}",
            ipv4,
            ipv6,
            params.vpc_id.unwrap(),
            vpc_cidr,
            gw_mac
        );

        // Configure eth0 with VPC addresses
        if let Err(e) = configure_vpc_networking(ipv4, ipv6, vpc_cidr, gw_mac) {
            log_error!("failed to configure VPC networking: {}", e);
        }
    } else if has_kernel_ip_param() {
        // Kernel ip= parameter handles networking (Firecracker legacy path)
        log_info!("kernel ip= parameter present — skipping DHCP");
    } else {
        // No VPC params, no kernel ip= → use DHCP (macOS Virtualization.framework NAT)
        log_info!("no VPC boot params — attempting DHCP on eth0");
        match crate::dhcp::do_dhcp("eth0") {
            Ok(lease) => {
                // Apply the lease to eth0
                if let Err(e) = apply_dhcp_lease(&lease) {
                    log_error!("failed to apply DHCP lease: {}", e);
                }
            }
            Err(e) => {
                log_error!("DHCP failed: {} — networking may be unavailable", e);
            }
        }
    }

    Ok(())
}

/// Check if the kernel ip= parameter was given on the cmdline.
#[cfg(target_os = "linux")]
fn has_kernel_ip_param() -> bool {
    std::fs::read_to_string("/proc/cmdline")
        .map(|s| s.split_whitespace().any(|tok| tok.starts_with("ip=")))
        .unwrap_or(false)
}

/// Apply a DHCP lease: set IP + netmask, add default route, write /etc/resolv.conf.
#[cfg(target_os = "linux")]
fn apply_dhcp_lease(lease: &crate::dhcp::DhcpLease) -> Result<(), Box<dyn std::error::Error>> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err("socket() failed".into());
    }

    // 1. Set IPv4 address
    set_ipv4_addr(sock, "eth0", &lease.ip)?;
    log_info!("eth0: IP set to {}", lease.ip);

    // 2. Set netmask
    let mask = if lease.prefix_len == 0 {
        0u32
    } else {
        !0u32 << (32 - lease.prefix_len)
    };
    set_ipv4_netmask(sock, "eth0", mask)?;
    log_info!("eth0: netmask set to /{}", lease.prefix_len);

    // 3. Add default route via gateway
    if let Some(ref gw) = lease.gateway {
        add_default_route(sock, gw)?;
        log_info!("eth0: default route via {}", gw);
    }

    unsafe { libc::close(sock) };

    // 4. Write /etc/resolv.conf
    write_resolv_conf(&lease.dns_servers);

    Ok(())
}

/// Write /etc/resolv.conf with the given DNS servers.
/// Falls back to 8.8.8.8 + 8.8.4.4 if no servers provided.
#[cfg(target_os = "linux")]
fn write_resolv_conf(dns_servers: &[String]) {
    let servers = if dns_servers.is_empty() {
        vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()]
    } else {
        dns_servers.to_vec()
    };

    let content: String = servers
        .iter()
        .map(|s| format!("nameserver {}", s))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    match std::fs::write("/etc/resolv.conf", &content) {
        Ok(_) => log_info!("wrote /etc/resolv.conf: {:?}", servers),
        Err(e) => log_error!("failed to write /etc/resolv.conf: {}", e),
    }
}

/// Configure eth0 with VPC IPv4/IPv6 addresses, default route, and static ARP.
#[cfg(target_os = "linux")]
fn configure_vpc_networking(
    ipv4: &str,
    ipv6: &str,
    vpc_cidr: &str,
    gw_mac: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Parse VPC CIDR to get netmask
    let (_network_str, prefix_len) = vpc_cidr.split_once('/').ok_or("invalid vpc_cidr format")?;
    let prefix_len: u32 = prefix_len.parse()?;
    let mask = if prefix_len == 0 {
        0u32
    } else {
        !0u32 << (32 - prefix_len)
    };

    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err("socket() failed".into());
    }

    // 1. Set IPv4 address on eth0
    set_ipv4_addr(sock, "eth0", ipv4)?;
    log_info!("eth0: IPv4 address set to {}", ipv4);

    // 2. Set IPv4 netmask
    set_ipv4_netmask(sock, "eth0", mask)?;
    log_info!("eth0: netmask set to /{}", prefix_len);

    // 3. Derive gateway IP: first IP in the /30 link subnet.
    //    The TAP uses a link-local /30: gateway is at .1 of the /30 subnet.
    //    We compute it from the VPC CIDR network as network.0.0.1 — but actually
    //    the gateway IP doesn't matter for ARP since we use static ARP.
    //    We use 169.254.0.1 as a synthetic link-local gateway.
    let gw_ip = "169.254.0.1";
    add_default_route(sock, gw_ip)?;
    log_info!("eth0: default route via {}", gw_ip);

    // 4. Add static ARP entry: map gateway IP to well-known MAC.
    //    Critical because the TAP has no IPv4 address — normal ARP won't work.
    let mac_bytes = parse_mac(gw_mac)?;
    add_static_arp(sock, "eth0", gw_ip, &mac_bytes)?;
    log_info!("eth0: static ARP {} → {}", gw_ip, gw_mac);

    unsafe { libc::close(sock) };

    // 5. Add Ghost IPv6 address to eth0
    add_ipv6_addr("eth0", ipv6)?;
    log_info!("eth0: IPv6 address set to {}", ipv6);

    Ok(())
}

/// Set IPv4 address on an interface using SIOCSIFADDR ioctl.
#[cfg(target_os = "linux")]
fn set_ipv4_addr(sock: i32, iface: &str, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    set_ifr_name(&mut ifr, iface);

    let ip = parse_ipv4(addr)?;
    let sa = make_sockaddr_in(ip);
    unsafe {
        std::ptr::copy_nonoverlapping(
            &sa as *const libc::sockaddr_in as *const u8,
            &mut ifr.ifr_ifru as *mut _ as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }

    if unsafe { libc::ioctl(sock, libc::SIOCSIFADDR as _, &ifr) } < 0 {
        return Err(format!(
            "SIOCSIFADDR failed for {}: {}",
            iface,
            std::io::Error::last_os_error()
        )
        .into());
    }
    Ok(())
}

/// Set IPv4 netmask on an interface using SIOCSIFNETMASK ioctl.
#[cfg(target_os = "linux")]
fn set_ipv4_netmask(sock: i32, iface: &str, mask: u32) -> Result<(), Box<dyn std::error::Error>> {
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    set_ifr_name(&mut ifr, iface);

    let sa = make_sockaddr_in(mask.to_be());
    unsafe {
        std::ptr::copy_nonoverlapping(
            &sa as *const libc::sockaddr_in as *const u8,
            &mut ifr.ifr_ifru as *mut _ as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }

    if unsafe { libc::ioctl(sock, libc::SIOCSIFNETMASK as _, &ifr) } < 0 {
        return Err(format!(
            "SIOCSIFNETMASK failed for {}: {}",
            iface,
            std::io::Error::last_os_error()
        )
        .into());
    }
    Ok(())
}

/// Add a default route via gateway IP using SIOCADDRT ioctl.
#[cfg(target_os = "linux")]
fn add_default_route(sock: i32, gateway: &str) -> Result<(), Box<dyn std::error::Error>> {
    let gw_ip = parse_ipv4(gateway)?;

    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };

    // Destination: 0.0.0.0
    let dst = make_sockaddr_in(0);
    unsafe {
        std::ptr::copy_nonoverlapping(
            &dst as *const libc::sockaddr_in as *const u8,
            &mut rt.rt_dst as *mut libc::sockaddr as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }

    // Gateway
    let gw = make_sockaddr_in(gw_ip);
    unsafe {
        std::ptr::copy_nonoverlapping(
            &gw as *const libc::sockaddr_in as *const u8,
            &mut rt.rt_gateway as *mut libc::sockaddr as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }

    // Netmask: 0.0.0.0
    let mask = make_sockaddr_in(0);
    unsafe {
        std::ptr::copy_nonoverlapping(
            &mask as *const libc::sockaddr_in as *const u8,
            &mut rt.rt_genmask as *mut libc::sockaddr as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }

    rt.rt_flags = (libc::RTF_UP | libc::RTF_GATEWAY) as u16;

    if unsafe { libc::ioctl(sock, libc::SIOCADDRT as _, &rt) } < 0 {
        let err = std::io::Error::last_os_error();
        // Ignore "File exists" (route already present)
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(format!("SIOCADDRT failed: {}", err).into());
        }
    }
    Ok(())
}

/// Add a static ARP entry using SIOCSARP ioctl.
#[cfg(target_os = "linux")]
fn add_static_arp(
    sock: i32,
    iface: &str,
    ip: &str,
    mac: &[u8; 6],
) -> Result<(), Box<dyn std::error::Error>> {
    let ip_addr = parse_ipv4(ip)?;

    let mut arp: libc::arpreq = unsafe { std::mem::zeroed() };

    // Set protocol address (IP)
    let sa = make_sockaddr_in(ip_addr);
    unsafe {
        std::ptr::copy_nonoverlapping(
            &sa as *const libc::sockaddr_in as *const u8,
            &mut arp.arp_pa as *mut libc::sockaddr as *mut u8,
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }

    // Set hardware address (MAC)
    arp.arp_ha.sa_family = libc::ARPHRD_ETHER;
    for i in 0..6 {
        arp.arp_ha.sa_data[i] = mac[i] as _;
    }

    // Set flags: permanent + complete
    arp.arp_flags = libc::ATF_PERM | libc::ATF_COM;

    // Set device name
    let name_bytes = iface.as_bytes();
    let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            arp.arp_dev.as_mut_ptr() as *mut u8,
            copy_len,
        );
    }

    if unsafe { libc::ioctl(sock, libc::SIOCSARP as _, &arp) } < 0 {
        return Err(format!(
            "SIOCSARP failed for {} → {}: {}",
            ip,
            iface,
            std::io::Error::last_os_error()
        )
        .into());
    }
    Ok(())
}

/// Add an IPv6 address to an interface.
#[cfg(target_os = "linux")]
fn add_ipv6_addr(iface: &str, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sock6 = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, 0) };
    if sock6 < 0 {
        return Err("socket(AF_INET6) failed".into());
    }

    // Get interface index
    let ifindex = get_ifindex(iface)?;

    // Parse IPv6 address
    let ipv6_bytes = parse_ipv6(addr)?;

    // Use in6_ifreq structure for IPv6 address assignment
    #[repr(C)]
    struct In6Ifreq {
        ifr6_addr: libc::in6_addr,
        ifr6_prefixlen: u32,
        ifr6_ifindex: i32,
    }

    let req = In6Ifreq {
        ifr6_addr: libc::in6_addr {
            s6_addr: ipv6_bytes,
        },
        ifr6_prefixlen: 128,
        ifr6_ifindex: ifindex as i32,
    };

    // SIOCSIFADDR for IPv6
    const SIOCSIFADDR_V6: libc::c_ulong = 0x8916;
    if unsafe { libc::ioctl(sock6, SIOCSIFADDR_V6 as _, &req) } < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(sock6) };
        // Ignore "File exists" (address already assigned)
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(format!("SIOCSIFADDR(v6) failed for {}: {}", iface, err).into());
        }
    }

    unsafe { libc::close(sock6) };
    Ok(())
}

/// Bring a network interface up using raw socket ioctl (no `ip` binary needed).
#[cfg(target_os = "linux")]
fn bring_interface_up(iface: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err(format!("socket() failed for {}", iface).into());
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    set_ifr_name(&mut ifr, iface);

    // Get current flags
    if unsafe { libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut ifr) } < 0 {
        unsafe { libc::close(sock) };
        return Err(format!("SIOCGIFFLAGS failed for {}", iface).into());
    }

    // Set IFF_UP | IFF_RUNNING
    unsafe {
        ifr.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as i16;
    }

    let ret = unsafe { libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) };
    unsafe { libc::close(sock) };

    if ret < 0 {
        return Err(format!("SIOCSIFFLAGS failed for {}", iface).into());
    }

    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn set_ifr_name(ifr: &mut libc::ifreq, iface: &str) {
    let name_bytes = iface.as_bytes();
    let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            copy_len,
        );
    }
}

/// Parse dotted-quad IPv4 address to network-byte-order u32.
#[cfg(target_os = "linux")]
fn parse_ipv4(addr: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let parts: Vec<u8> = addr
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() != 4 {
        return Err(format!("invalid IPv4: {}", addr).into());
    }
    Ok(u32::from_ne_bytes([parts[0], parts[1], parts[2], parts[3]]))
}

/// Parse colon-hex IPv6 address to 16-byte array.
#[cfg(target_os = "linux")]
fn parse_ipv6(addr: &str) -> Result<[u8; 16], Box<dyn std::error::Error>> {
    parse_ipv6_manual(addr)
}

/// Parse IPv6 address manually (handles :: expansion).
#[cfg(target_os = "linux")]
pub(crate) fn parse_ipv6_manual(addr: &str) -> Result<[u8; 16], Box<dyn std::error::Error>> {
    let mut result = [0u8; 16];

    // Split on :: to handle zero-expansion
    let (left, right) = if let Some(pos) = addr.find("::") {
        (&addr[..pos], &addr[pos + 2..])
    } else {
        (addr, "")
    };

    let mut groups = Vec::new();

    // Parse left side
    if !left.is_empty() {
        for g in left.split(':') {
            groups.push(u16::from_str_radix(g, 16)?);
        }
    }

    // Parse right side
    let mut right_groups = Vec::new();
    if !right.is_empty() {
        for g in right.split(':') {
            right_groups.push(u16::from_str_radix(g, 16)?);
        }
    }

    // Fill zeros in between
    let total = groups.len() + right_groups.len();
    if addr.contains("::") {
        let zeros = 8 - total;
        for _ in 0..zeros {
            groups.push(0);
        }
    }
    groups.extend(right_groups);

    if groups.len() != 8 {
        return Err(format!("invalid IPv6 (wrong group count): {}", addr).into());
    }

    for (i, g) in groups.iter().enumerate() {
        let bytes = g.to_be_bytes();
        result[i * 2] = bytes[0];
        result[i * 2 + 1] = bytes[1];
    }

    Ok(result)
}

/// Parse MAC address string like "02:fc:00:00:00:01" to 6-byte array.
#[cfg(target_os = "linux")]
fn parse_mac(mac: &str) -> Result<[u8; 6], Box<dyn std::error::Error>> {
    let parts: Vec<u8> = mac
        .split(':')
        .map(|s| u8::from_str_radix(s, 16))
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() != 6 {
        return Err(format!("invalid MAC: {}", mac).into());
    }
    let mut arr = [0u8; 6];
    arr.copy_from_slice(&parts);
    Ok(arr)
}

/// Create a sockaddr_in with the given network-byte-order address.
#[cfg(target_os = "linux")]
fn make_sockaddr_in(addr: u32) -> libc::sockaddr_in {
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_addr.s_addr = addr;
    sa
}

/// Get interface index from sysfs.
#[cfg(target_os = "linux")]
fn get_ifindex(iface: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let path = format!("/sys/class/net/{}/ifindex", iface);
    let content = std::fs::read_to_string(&path)?;
    Ok(content.trim().parse()?)
}
