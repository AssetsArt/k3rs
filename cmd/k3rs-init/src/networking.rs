use std::io::Write;
use std::path::Path;

/// Setup networking: bring up loopback and eth0.
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
    let name_bytes = iface.as_bytes();
    let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            copy_len,
        );
    }

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
