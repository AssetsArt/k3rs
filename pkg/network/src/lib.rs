pub mod cni;
pub mod dns;
pub mod wireguard;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;
