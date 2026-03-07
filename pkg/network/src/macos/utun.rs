//! macOS utun device creation for external IPv4 routing.
//!
//! Creates a point-to-point tunnel device using the macOS kernel control API.
//! The utun device carries raw IP packets (no Ethernet header) with a 4-byte
//! AF protocol header prepended by the kernel.

use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};

// ── macOS kernel control constants (not in libc crate) ───────────

const AF_SYSTEM: libc::c_int = 32;
const SYSPROTO_CONTROL: libc::c_int = 2;
const AF_SYS_CONTROL: u16 = 2;
const UTUN_CONTROL_NAME: &[u8] = b"com.apple.net.utun_control\0";
const UTUN_OPT_IFNAME: libc::c_int = 2;

/// `CTLIOCGINFO` = `_IOWR('N', 3, struct ctl_info)`
/// On macOS aarch64: sizeof(ctl_info) = 100 (4 + 96), 'N' = 0x4e
/// _IOWR = 0xC0000000 | (size << 16) | (group << 8) | num
/// = 0xC0000000 | (100 << 16) | (0x4e << 8) | 3
/// = 0xC0644E03
const CTLIOCGINFO: libc::c_ulong = 0xC064_4E03;

// ── FFI structs ──────────────────────────────────────────────────

/// `struct ctl_info` from <sys/kern_control.h>
#[repr(C)]
struct CtlInfo {
    ctl_id: u32,
    ctl_name: [u8; 96],
}

/// `struct sockaddr_ctl` from <sys/kern_control.h>
#[repr(C)]
struct SockaddrCtl {
    sc_len: u8,
    sc_family: u8,
    ss_sysaddr: u16,
    sc_id: u32,
    sc_unit: u32,
    sc_reserved: [u32; 5],
}

// ── Utun device ──────────────────────────────────────────────────

/// An owned macOS utun device.
pub struct Utun {
    fd: OwnedFd,
    /// Interface name (e.g. "utun5")
    pub name: String,
}

impl Utun {
    /// Create a new utun device. Returns the device with an auto-assigned unit number.
    ///
    /// Requires root or appropriate entitlements.
    pub fn create() -> io::Result<Self> {
        // 1. Create a system control socket
        let fd = unsafe { libc::socket(AF_SYSTEM, libc::SOCK_DGRAM, SYSPROTO_CONTROL) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        // 2. Look up the control ID for utun
        let mut info = CtlInfo {
            ctl_id: 0,
            ctl_name: [0u8; 96],
        };
        info.ctl_name[..UTUN_CONTROL_NAME.len()].copy_from_slice(UTUN_CONTROL_NAME);

        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), CTLIOCGINFO, &mut info) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // 3. Connect to create the utun device (unit 0 = auto-assign)
        let addr = SockaddrCtl {
            sc_len: std::mem::size_of::<SockaddrCtl>() as u8,
            sc_family: AF_SYSTEM as u8,
            ss_sysaddr: AF_SYS_CONTROL,
            sc_id: info.ctl_id,
            sc_unit: 0, // auto-assign
            sc_reserved: [0; 5],
        };

        let ret = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                &addr as *const SockaddrCtl as *const libc::sockaddr,
                std::mem::size_of::<SockaddrCtl>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // 4. Get the assigned interface name
        let name = Self::get_ifname(fd.as_raw_fd())?;

        Ok(Utun { fd, name })
    }

    /// Read the interface name via getsockopt.
    fn get_ifname(fd: RawFd) -> io::Result<String> {
        let mut buf = [0u8; 32];
        let mut len: libc::socklen_t = buf.len() as libc::socklen_t;

        let ret = unsafe {
            libc::getsockopt(
                fd,
                SYSPROTO_CONTROL,
                UTUN_OPT_IFNAME,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut len,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        let name = std::str::from_utf8(&buf[..len as usize])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid utun name"))?
            .trim_end_matches('\0')
            .to_string();

        Ok(name)
    }
}

impl AsRawFd for Utun {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
