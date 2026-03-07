//! Userspace Ethernet switch for macOS VPC networking.
//!
//! Manages raw Ethernet frame forwarding between VM sockets created via
//! `socketpair(AF_UNIX, SOCK_DGRAM)` + `VZFileHandleNetworkDeviceAttachment`.
//!
//! Responsibilities:
//! - ARP proxy: respond to ARP requests with the gateway MAC
//! - IPv4 routing: forward between same-VPC pods or to external via utun
//! - VPC isolation: enforce that only same-VPC (or peered) pods can communicate
//! - DNS proxy: intercept DNS queries and forward to k3rs DNS server

#[cfg(target_os = "macos")]
mod inner {
    use std::collections::HashMap;
    use std::io;
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
    use std::sync::Arc;

    use tokio::io::unix::AsyncFd;
    use tokio::sync::RwLock;
    use tracing::{debug, info, trace, warn};

    /// Well-known gateway MAC used by k3rs VMs (matches k3rs.gw_mac kernel param).
    const GATEWAY_MAC: [u8; 6] = pkg_constants::network::GATEWAY_MAC;

    /// DNS proxy IP inside the VM's link-local range.
    const DNS_PROXY_IP: Ipv4Addr = Ipv4Addr::new(169, 254, 0, 53); // matches pkg_constants::network::DNS_PROXY_IPV4

    /// EtherType constants
    const ETH_P_ARP: u16 = 0x0806;
    const ETH_P_IPV4: u16 = 0x0800;

    /// A registered VM endpoint in the switch.
    struct Endpoint {
        /// AsyncFd wrapping the host-side datagram socket
        fd: AsyncFd<RawFdWrapper>,
        /// Raw FD for send/recv (kept alive by OwnedFd in fd_owner)
        raw_fd: RawFd,
        /// Owns the file descriptor lifetime
        _fd_owner: OwnedFd,
        guest_ipv4: Ipv4Addr,
        vpc_id: u16,
        /// VM's virtio-net MAC (learned from first frame or assigned)
        vm_mac: [u8; 6],
    }

    /// Wrapper to impl AsRawFd for AsyncFd
    struct RawFdWrapper(RawFd);
    impl AsRawFd for RawFdWrapper {
        fn as_raw_fd(&self) -> RawFd {
            self.0
        }
    }

    /// Userspace Ethernet switch for macOS VM networking.
    pub struct MacSwitch {
        /// VM endpoints keyed by container/pod ID
        endpoints: Arc<RwLock<HashMap<String, Endpoint>>>,
        /// Reverse lookup: guest IPv4 → pod ID
        ip_to_id: Arc<RwLock<HashMap<Ipv4Addr, String>>>,
        /// DNS server address on the host
        dns_addr: SocketAddr,
        /// utun device for external IPv4 routing (None if creation failed)
        utun: Option<Arc<crate::utun::Utun>>,
    }

    impl Drop for MacSwitch {
        fn drop(&mut self) {
            if let Some(ref utun) = self.utun {
                crate::pfnat::teardown_nat(&utun.name, pkg_constants::network::DEFAULT_POD_CIDR);
            }
        }
    }

    impl MacSwitch {
        /// Create a new switch. `dns_port` is the port of the k3rs DNS server on localhost.
        pub fn new(dns_port: u16) -> Self {
            // Create utun device for external IPv4 routing (best-effort)
            let utun = match crate::utun::Utun::create() {
                Ok(u) => {
                    info!("[switch] created utun device: {}", u.name);

                    // Set non-blocking for poll loop
                    unsafe {
                        let flags = libc::fcntl(u.as_raw_fd(), libc::F_GETFL);
                        libc::fcntl(u.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
                    }

                    // Setup NAT: ifconfig, route, pfctl
                    if let Err(e) = crate::pfnat::setup_nat(
                        &u.name,
                        pkg_constants::network::DEFAULT_POD_CIDR,
                    ) {
                        warn!(
                            "[switch] pfctl NAT setup failed: {} (external access unavailable)",
                            e
                        );
                    }

                    Some(Arc::new(u))
                }
                Err(e) => {
                    warn!(
                        "[switch] failed to create utun: {} (external access unavailable)",
                        e
                    );
                    None
                }
            };

            Self {
                endpoints: Arc::new(RwLock::new(HashMap::new())),
                ip_to_id: Arc::new(RwLock::new(HashMap::new())),
                dns_addr: SocketAddr::new(
                    std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
                    dns_port,
                ),
                utun,
            }
        }

        /// Register a VM with the switch.
        pub async fn add_vm(
            &self,
            id: String,
            socket: OwnedFd,
            guest_ipv4: Ipv4Addr,
            vpc_id: u16,
        ) -> io::Result<()> {
            let raw_fd = socket.as_raw_fd();
            // Set non-blocking for tokio AsyncFd
            unsafe {
                let flags = libc::fcntl(raw_fd, libc::F_GETFL);
                libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }

            let async_fd = AsyncFd::new(RawFdWrapper(raw_fd))?;

            let endpoint = Endpoint {
                fd: async_fd,
                raw_fd,
                _fd_owner: socket,
                guest_ipv4,
                vpc_id,
                vm_mac: [0; 6], // learned on first frame
            };

            self.endpoints.write().await.insert(id.clone(), endpoint);
            self.ip_to_id.write().await.insert(guest_ipv4, id.clone());

            info!("[switch] registered VM {} (ip={}, vpc={})", id, guest_ipv4, vpc_id);
            Ok(())
        }

        /// Unregister a VM from the switch.
        pub async fn remove_vm(&self, id: &str) {
            if let Some(ep) = self.endpoints.write().await.remove(id) {
                self.ip_to_id.write().await.remove(&ep.guest_ipv4);
                info!("[switch] unregistered VM {} (ip={})", id, ep.guest_ipv4);
            }
        }

        /// Run the switch event loop. Spawns a task per VM that reads frames.
        /// Call this once; new VMs are picked up dynamically.
        pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
            let switch = self;
            tokio::spawn(async move {
                info!("[switch] macOS userspace switch started");
                // Poll loop: check for readable sockets every 1ms
                // A production switch would use epoll/kqueue, but for macOS VM
                // counts (typically <50 pods) this is fine.
                loop {
                    let readable_ids: Vec<String> = {
                        let eps = switch.endpoints.read().await;
                        let mut ready = Vec::new();
                        for (id, ep) in eps.iter() {
                            match ep.fd.readable().await {
                                Ok(mut guard) => {
                                    guard.clear_ready();
                                    ready.push(id.clone());
                                }
                                Err(_) => {}
                            }
                        }
                        ready
                    };

                    for id in readable_ids {
                        let mut buf = [0u8; 2048];
                        let n = {
                            let eps = switch.endpoints.read().await;
                            if let Some(ep) = eps.get(&id) {
                                let n = unsafe {
                                    libc::recv(
                                        ep.raw_fd,
                                        buf.as_mut_ptr() as *mut libc::c_void,
                                        buf.len(),
                                        libc::MSG_DONTWAIT,
                                    )
                                };
                                if n <= 0 {
                                    continue;
                                }
                                n as usize
                            } else {
                                continue;
                            }
                        };

                        if n < 14 {
                            continue; // too small for Ethernet header
                        }

                        let frame = &buf[..n];
                        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);

                        match ethertype {
                            ETH_P_ARP => {
                                switch.handle_arp(&id, frame).await;
                            }
                            ETH_P_IPV4 => {
                                switch.handle_ipv4(&id, frame).await;
                            }
                            _ => {
                                trace!("[switch] dropping frame with ethertype 0x{:04x} from {}", ethertype, id);
                            }
                        }
                    }

                    // Poll utun for incoming reply packets (non-blocking)
                    if let Some(ref utun) = switch.utun {
                        let mut buf = [0u8; 2048];
                        let n = unsafe {
                            libc::recv(
                                utun.as_raw_fd(),
                                buf.as_mut_ptr() as *mut libc::c_void,
                                buf.len(),
                                libc::MSG_DONTWAIT,
                            )
                        };
                        if n > 4 {
                            let n = n as usize;
                            // Skip 4-byte AF protocol header → raw IP packet
                            let ip_packet = &buf[4..n];
                            switch.handle_utun_reply(ip_packet).await;
                        }
                    }

                    // Small yield to avoid busy-spinning
                    tokio::task::yield_now().await;
                }
            })
        }

        /// Handle an ARP request: reply with gateway MAC for any known IP.
        async fn handle_arp(&self, src_id: &str, frame: &[u8]) {
            // ARP packet layout (after 14-byte Ethernet header):
            //   0-1: hardware type (0x0001 = Ethernet)
            //   2-3: protocol type (0x0800 = IPv4)
            //   4:   hw addr len (6)
            //   5:   proto addr len (4)
            //   6-7: opcode (1=request, 2=reply)
            //   8-13:  sender MAC
            //   14-17: sender IP
            //   18-23: target MAC
            //   24-27: target IP
            if frame.len() < 14 + 28 {
                return;
            }
            let arp = &frame[14..];
            let opcode = u16::from_be_bytes([arp[6], arp[7]]);
            if opcode != 1 {
                return; // only handle ARP requests
            }

            let sender_mac = &arp[8..14];
            let sender_ip = Ipv4Addr::new(arp[14], arp[15], arp[16], arp[17]);
            let target_ip = Ipv4Addr::new(arp[24], arp[25], arp[26], arp[27]);

            // Learn sender's MAC
            {
                let mut eps = self.endpoints.write().await;
                if let Some(ep) = eps.get_mut(src_id) {
                    ep.vm_mac.copy_from_slice(sender_mac);
                }
            }

            debug!("[switch] ARP request from {} ({}) for {}", src_id, sender_ip, target_ip);

            // Build ARP reply
            let mut reply = vec![0u8; 14 + 28];
            // Ethernet header
            reply[0..6].copy_from_slice(sender_mac); // dst = requester
            reply[6..12].copy_from_slice(&GATEWAY_MAC); // src = gateway
            reply[12..14].copy_from_slice(&ETH_P_ARP.to_be_bytes());
            // ARP payload
            reply[14..16].copy_from_slice(&[0x00, 0x01]); // hw type: Ethernet
            reply[16..18].copy_from_slice(&[0x08, 0x00]); // proto: IPv4
            reply[18] = 6; // hw addr len
            reply[19] = 4; // proto addr len
            reply[20..22].copy_from_slice(&[0x00, 0x02]); // opcode: reply
            reply[22..28].copy_from_slice(&GATEWAY_MAC); // sender MAC = gateway
            reply[28..32].copy_from_slice(&target_ip.octets()); // sender IP = target
            reply[32..38].copy_from_slice(sender_mac); // target MAC = requester
            reply[38..42].copy_from_slice(&sender_ip.octets()); // target IP = requester

            // Send reply back to the requesting VM
            let eps = self.endpoints.read().await;
            if let Some(ep) = eps.get(src_id) {
                let ret = unsafe {
                    libc::send(
                        ep.raw_fd,
                        reply.as_ptr() as *const libc::c_void,
                        reply.len(),
                        0,
                    )
                };
                if ret < 0 {
                    warn!("[switch] failed to send ARP reply to {}: {}", src_id, io::Error::last_os_error());
                }
            }
        }

        /// Handle an IPv4 frame: route to destination VM or external.
        async fn handle_ipv4(&self, src_id: &str, frame: &[u8]) {
            if frame.len() < 14 + 20 {
                return; // too small for IPv4 header
            }

            let ip_header = &frame[14..];
            let dst_ip = Ipv4Addr::new(ip_header[16], ip_header[17], ip_header[18], ip_header[19]);

            // Get source VPC ID
            let src_vpc_id = {
                let eps = self.endpoints.read().await;
                match eps.get(src_id) {
                    Some(ep) => ep.vpc_id,
                    None => return,
                }
            };

            // Check if destination is DNS proxy
            if dst_ip == DNS_PROXY_IP {
                self.handle_dns(src_id, frame).await;
                return;
            }

            // Check if destination is a known pod
            let dst_id = self.ip_to_id.read().await.get(&dst_ip).cloned();
            if let Some(ref dst_id) = dst_id {
                let eps = self.endpoints.read().await;
                if let Some(dst_ep) = eps.get(dst_id.as_str()) {
                    // VPC isolation check
                    if dst_ep.vpc_id != src_vpc_id {
                        trace!("[switch] VPC isolation: dropping {} → {} (vpc {} → {})", src_id, dst_id, src_vpc_id, dst_ep.vpc_id);
                        return;
                    }

                    // Rewrite MACs and forward
                    let mut fwd_frame = frame.to_vec();
                    fwd_frame[0..6].copy_from_slice(&dst_ep.vm_mac); // dst MAC = target VM
                    fwd_frame[6..12].copy_from_slice(&GATEWAY_MAC); // src MAC = gateway

                    let ret = unsafe {
                        libc::send(
                            dst_ep.raw_fd,
                            fwd_frame.as_ptr() as *const libc::c_void,
                            fwd_frame.len(),
                            0,
                        )
                    };
                    if ret < 0 {
                        warn!("[switch] failed to forward to {}: {}", dst_id, io::Error::last_os_error());
                    } else {
                        trace!("[switch] forwarded {} bytes {} → {}", fwd_frame.len(), src_id, dst_id);
                    }
                    return;
                }
            }

            // Destination is external — forward via utun
            if let Some(ref utun) = self.utun {
                let ip_packet = &frame[14..]; // strip Ethernet header

                // utun requires a 4-byte AF protocol header before the IP packet
                let af_inet = (libc::AF_INET as u32).to_be_bytes();
                let mut utun_buf = Vec::with_capacity(4 + ip_packet.len());
                utun_buf.extend_from_slice(&af_inet);
                utun_buf.extend_from_slice(ip_packet);

                let ret = unsafe {
                    libc::write(
                        utun.as_raw_fd(),
                        utun_buf.as_ptr() as *const libc::c_void,
                        utun_buf.len(),
                    )
                };
                if ret < 0 {
                    warn!(
                        "[switch] utun write failed: {}",
                        io::Error::last_os_error()
                    );
                } else {
                    trace!(
                        "[switch] external: {} → {} ({} bytes via utun)",
                        src_id,
                        dst_ip,
                        ip_packet.len()
                    );
                }
            } else {
                trace!(
                    "[switch] external traffic from {} to {} (utun unavailable)",
                    src_id,
                    dst_ip
                );
            }
        }

        /// Handle DNS query: extract UDP payload and forward to k3rs DNS server.
        async fn handle_dns(&self, src_id: &str, frame: &[u8]) {
            if frame.len() < 14 + 20 + 8 {
                return; // need Ethernet + IPv4 + UDP headers
            }

            let ip_header = &frame[14..];
            let ihl = ((ip_header[0] & 0x0f) as usize) * 4;
            let protocol = ip_header[9];
            if protocol != 17 {
                return; // not UDP
            }

            let udp_start = 14 + ihl;
            if frame.len() < udp_start + 8 {
                return;
            }
            let udp = &frame[udp_start..];
            let src_port = u16::from_be_bytes([udp[0], udp[1]]);
            let dst_port = u16::from_be_bytes([udp[2], udp[3]]);
            if dst_port != 53 {
                return;
            }

            let dns_payload = &frame[udp_start + 8..];
            if dns_payload.is_empty() {
                return;
            }

            debug!("[switch] DNS query from {} (port {}), {} bytes", src_id, src_port, dns_payload.len());

            // Forward DNS query to host DNS server via blocking UDP (in spawn_blocking)
            let dns_addr = self.dns_addr;
            let payload = dns_payload.to_vec();
            let response = tokio::task::spawn_blocking(move || -> io::Result<Vec<u8>> {
                let sock = UdpSocket::bind("127.0.0.1:0")?;
                sock.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
                sock.send_to(&payload, dns_addr)?;
                let mut buf = vec![0u8; 1500];
                let (n, _) = sock.recv_from(&mut buf)?;
                buf.truncate(n);
                Ok(buf)
            })
            .await;

            let dns_response = match response {
                Ok(Ok(data)) => data,
                Ok(Err(e)) => {
                    warn!("[switch] DNS proxy error: {}", e);
                    return;
                }
                Err(e) => {
                    warn!("[switch] DNS proxy task error: {}", e);
                    return;
                }
            };

            // Build response Ethernet + IP + UDP frame
            let src_ip_bytes = &frame[14 + 12..14 + 16]; // original src IP
            let src_ip = Ipv4Addr::new(src_ip_bytes[0], src_ip_bytes[1], src_ip_bytes[2], src_ip_bytes[3]);

            let eps = self.endpoints.read().await;
            let ep = match eps.get(src_id) {
                Some(ep) => ep,
                None => return,
            };

            let total_ip_len = 20 + 8 + dns_response.len();
            let mut resp_frame = vec![0u8; 14 + total_ip_len];

            // Ethernet header
            resp_frame[0..6].copy_from_slice(&ep.vm_mac); // dst = VM
            resp_frame[6..12].copy_from_slice(&GATEWAY_MAC); // src = gateway
            resp_frame[12..14].copy_from_slice(&ETH_P_IPV4.to_be_bytes());

            // IPv4 header
            let ip = &mut resp_frame[14..14 + 20];
            ip[0] = 0x45; // version=4, IHL=5
            ip[2..4].copy_from_slice(&(total_ip_len as u16).to_be_bytes());
            ip[8] = 64; // TTL
            ip[9] = 17; // UDP
            ip[12..16].copy_from_slice(&DNS_PROXY_IP.octets()); // src = DNS proxy
            ip[16..20].copy_from_slice(&src_ip.octets()); // dst = VM
            // Compute IPv4 checksum
            let cksum = ipv4_checksum(&resp_frame[14..14 + 20]);
            resp_frame[14 + 10..14 + 12].copy_from_slice(&cksum.to_be_bytes());

            // UDP header
            let udp_start = 14 + 20;
            let udp_len = (8 + dns_response.len()) as u16;
            resp_frame[udp_start..udp_start + 2].copy_from_slice(&53u16.to_be_bytes()); // src port = 53
            resp_frame[udp_start + 2..udp_start + 4].copy_from_slice(&src_port.to_be_bytes()); // dst port
            resp_frame[udp_start + 4..udp_start + 6].copy_from_slice(&udp_len.to_be_bytes());
            // UDP checksum = 0 (optional for IPv4)

            // DNS payload
            resp_frame[udp_start + 8..].copy_from_slice(&dns_response);

            let ret = unsafe {
                libc::send(
                    ep.raw_fd,
                    resp_frame.as_ptr() as *const libc::c_void,
                    resp_frame.len(),
                    0,
                )
            };
            if ret < 0 {
                warn!("[switch] failed to send DNS response to {}: {}", src_id, io::Error::last_os_error());
            } else {
                debug!("[switch] DNS response sent to {} ({} bytes)", src_id, resp_frame.len());
            }
        }

        /// Handle a reply packet from utun: wrap in Ethernet and deliver to the destination VM.
        async fn handle_utun_reply(&self, ip_packet: &[u8]) {
            if ip_packet.len() < 20 {
                return; // too small for IPv4 header
            }

            // Parse destination IP from IP header (bytes 16-19)
            let dst_ip = Ipv4Addr::new(
                ip_packet[16],
                ip_packet[17],
                ip_packet[18],
                ip_packet[19],
            );

            // Look up which VM owns this IP
            let vm_id = match self.ip_to_id.read().await.get(&dst_ip).cloned() {
                Some(id) => id,
                None => {
                    trace!("[switch] utun reply for unknown IP {}, dropping", dst_ip);
                    return;
                }
            };

            let eps = self.endpoints.read().await;
            let ep = match eps.get(&vm_id) {
                Some(ep) => ep,
                None => return,
            };

            // Build Ethernet frame: dst=VM_MAC, src=GATEWAY_MAC, type=IPv4
            let mut frame = Vec::with_capacity(14 + ip_packet.len());
            frame.extend_from_slice(&ep.vm_mac); // dst MAC = VM
            frame.extend_from_slice(&GATEWAY_MAC); // src MAC = gateway
            frame.extend_from_slice(&ETH_P_IPV4.to_be_bytes());
            frame.extend_from_slice(ip_packet);

            let ret = unsafe {
                libc::send(
                    ep.raw_fd,
                    frame.as_ptr() as *const libc::c_void,
                    frame.len(),
                    0,
                )
            };
            if ret < 0 {
                warn!(
                    "[switch] failed to send utun reply to {}: {}",
                    vm_id,
                    io::Error::last_os_error()
                );
            } else {
                trace!(
                    "[switch] utun reply: {} bytes → {} ({})",
                    ip_packet.len(),
                    vm_id,
                    dst_ip
                );
            }
        }
    }

    /// Compute IPv4 header checksum (RFC 1071).
    fn ipv4_checksum(header: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        for i in (0..header.len()).step_by(2) {
            let word = if i + 1 < header.len() {
                u16::from_be_bytes([header[i], header[i + 1]])
            } else {
                u16::from_be_bytes([header[i], 0])
            };
            // Skip checksum field (bytes 10-11)
            if i == 10 {
                continue;
            }
            sum += word as u32;
        }
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !(sum as u16)
    }
}

#[cfg(target_os = "macos")]
pub use inner::MacSwitch;
