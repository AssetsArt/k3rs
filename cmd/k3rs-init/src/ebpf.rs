//! eBPF SIIT translation for in-guest VMs.
//!
//! Loads the same SIIT eBPF programs used by OCI containers, but inside
//! the VM's own kernel. Maps are empty (separate kernel from host), so
//! the formula fallback in `siit_in` uses MY_VPC_NETWORK/MY_VPC_MASK
//! .rodata globals to compute Ghost IPv6 addresses.

#[cfg(all(target_os = "linux", feature = "ebpf"))]
const EBPF_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/k3rs-vpc-ebpf"));

/// Setup eBPF SIIT translation on eth0 inside the VM.
///
/// Reads VPC parameters from /proc/cmdline (already parsed by networking module).
/// If parameters are present, mounts bpffs, loads eBPF programs with .rodata
/// globals, and attaches SIIT translators to eth0.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
pub fn setup_ebpf() -> Result<(), Box<dyn std::error::Error>> {
    use aya::programs::tc::{self, SchedClassifier, TcAttachType};
    use aya::EbpfLoader;

    let params = crate::networking::parse_cmdline();
    if !params.is_complete() {
        log_info!("ebpf: no VPC boot params — skipping eBPF SIIT setup");
        return Ok(());
    }

    let ipv4_str = params.ipv4.as_ref().unwrap();
    let ipv6_str = params.ipv6.as_ref().unwrap();
    let vpc_id = params.vpc_id.unwrap();
    let vpc_cidr = params.vpc_cidr.as_ref().unwrap();

    // Parse addresses
    let ipv6_bytes = parse_ipv6_bytes(ipv6_str)?;
    let ipv4_host = parse_ipv4_host_order(ipv4_str)?;
    let (vpc_network, vpc_mask) = parse_cidr(vpc_cidr)?;

    // 1. Mount bpffs
    mount_bpffs()?;
    log_info!("ebpf: bpffs mounted at /sys/fs/bpf");

    // 2. Load eBPF binary with .rodata globals
    let mut ebpf = EbpfLoader::new()
        .set_global("MY_GHOST_IPV6", &ipv6_bytes, true)
        .set_global("MY_GUEST_IPV4", &ipv4_host, true)
        .set_global("MY_VPC_ID", &vpc_id, true)
        .set_global("MY_VPC_NETWORK", &vpc_network, true)
        .set_global("MY_VPC_MASK", &vpc_mask, true)
        .load(EBPF_BYTES)
        .map_err(|e| format!("failed to load eBPF programs: {}", e))?;

    // 3. Add clsact qdisc to eth0
    if let Err(e) = tc::qdisc_add_clsact("eth0") {
        let msg = format!("{}", e);
        if !msg.contains("exist") {
            return Err(format!("failed to add clsact qdisc to eth0: {}", e).into());
        }
    }

    // 4. Attach siit_in → eth0 Egress (app sends IPv4 out → translated to IPv6)
    let siit_in: &mut SchedClassifier = ebpf
        .program_mut("siit_in")
        .ok_or("siit_in program not found")?
        .try_into()
        .map_err(|e| format!("siit_in not a classifier: {}", e))?;
    siit_in
        .load()
        .map_err(|e| format!("siit_in load failed: {}", e))?;
    siit_in
        .attach("eth0", TcAttachType::Egress)
        .map_err(|e| format!("siit_in attach failed: {}", e))?;

    // 5. Attach siit_out → eth0 Ingress (IPv6 arrives → translated to IPv4 for app)
    let siit_out: &mut SchedClassifier = ebpf
        .program_mut("siit_out")
        .ok_or("siit_out program not found")?
        .try_into()
        .map_err(|e| format!("siit_out not a classifier: {}", e))?;
    siit_out
        .load()
        .map_err(|e| format!("siit_out load failed: {}", e))?;
    siit_out
        .attach("eth0", TcAttachType::Ingress)
        .map_err(|e| format!("siit_out attach failed: {}", e))?;

    // Keep the Ebpf instance alive — leak it so programs stay attached.
    // The VM lifetime == program lifetime, so this is fine.
    std::mem::forget(ebpf);

    log_info!(
        "ebpf: SIIT attached on eth0 (ipv4={} ipv6={} vpc_id={} cidr={})",
        ipv4_str,
        ipv6_str,
        vpc_id,
        vpc_cidr
    );

    Ok(())
}

/// No-op when eBPF feature is disabled.
#[cfg(not(all(target_os = "linux", feature = "ebpf")))]
pub fn setup_ebpf() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Mount bpffs at /sys/fs/bpf.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn mount_bpffs() -> Result<(), Box<dyn std::error::Error>> {
    let target = "/sys/fs/bpf";
    std::fs::create_dir_all(target)?;

    let src = std::ffi::CString::new("bpf")?;
    let tgt = std::ffi::CString::new(target)?;
    let fstype = std::ffi::CString::new("bpf")?;

    let ret = unsafe {
        libc::mount(
            src.as_ptr(),
            tgt.as_ptr(),
            fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        // Ignore "already mounted"
        if err.raw_os_error() != Some(libc::EBUSY) {
            return Err(format!("mount bpffs failed: {}", err).into());
        }
    }
    Ok(())
}

/// Parse IPv6 string to 16-byte array.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn parse_ipv6_bytes(addr: &str) -> Result<[u8; 16], Box<dyn std::error::Error>> {
    crate::networking::parse_ipv6_manual(addr)
}

/// Parse IPv4 dotted-quad to host-byte-order u32.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn parse_ipv4_host_order(addr: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let parts: Vec<u8> = addr
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() != 4 {
        return Err(format!("invalid IPv4: {}", addr).into());
    }
    Ok(u32::from_be_bytes([parts[0], parts[1], parts[2], parts[3]]))
}

/// Parse CIDR string like "10.0.1.0/24" into (network, mask) in host byte order.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn parse_cidr(cidr: &str) -> Result<(u32, u32), Box<dyn std::error::Error>> {
    let (addr_str, prefix_str) = cidr.split_once('/').ok_or("invalid CIDR format")?;
    let prefix_len: u32 = prefix_str.parse()?;
    if prefix_len > 32 {
        return Err(format!("invalid CIDR prefix: {}", prefix_len).into());
    }
    let network = parse_ipv4_host_order(addr_str)?;
    let mask = if prefix_len == 0 {
        0
    } else {
        !0u32 << (32 - prefix_len)
    };
    Ok((network, mask))
}
