//! Create a Linux VM configuration using Apple's Virtualization.framework.
//!
//! Configures:
//! - VZLinuxBootLoader (kernel + optional initrd)
//! - virtio-fs (rootfs directory shared as guest filesystem)
//! - virtio-net (NAT networking)
//! - virtio-console (serial output → log file or stdout)
//! - virtio-vsock (host ↔ guest exec channel)
//! - virtio-entropy (RNG)
//! - virtio-balloon (memory management)

use std::fs::File;
use std::path::Path;

use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_virtualization::{
    VZFileSerialPortAttachment, VZLinuxBootLoader, VZNATNetworkDeviceAttachment,
    VZSerialPortConfiguration, VZSharedDirectory, VZSingleDirectoryShare,
    VZVirtioConsoleDeviceSerialPortConfiguration, VZVirtioEntropyDeviceConfiguration,
    VZVirtioFileSystemDeviceConfiguration, VZVirtioNetworkDeviceConfiguration,
    VZVirtioSocketDeviceConfiguration, VZVirtioTraditionalMemoryBalloonDeviceConfiguration,
    VZVirtualMachine, VZVirtualMachineConfiguration,
};
use tracing::info;

use crate::VmConfig;

/// Create a fully configured Linux VM ready to start.
pub fn create_vm(config: &VmConfig) -> Retained<VZVirtualMachine> {
    info!("creating linux vm: id={}", config.id);
    let vz_config = create_vm_config(config);
    unsafe {
        vz_config.validateWithError().unwrap_or_else(|err| {
            panic!(
                "virtual machine config validation error, err={}",
                err.localizedDescription()
            )
        });
        VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &vz_config)
    }
}

fn create_vm_config(config: &VmConfig) -> Retained<VZVirtualMachineConfiguration> {
    unsafe {
        let vz_config = VZVirtualMachineConfiguration::new();

        // --- CPU & Memory ---
        let min_cpu = VZVirtualMachineConfiguration::minimumAllowedCPUCount();
        vz_config.setCPUCount(config.cpu_count.max(min_cpu));
        vz_config.setMemorySize((config.memory_mb.max(64)) * 1024 * 1024);

        // --- Boot Loader ---
        vz_config.setBootLoader(Some(&boot_loader(config)));

        // --- Network: virtio-net with NAT ---
        let net = VZVirtioNetworkDeviceConfiguration::new();
        net.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
        vz_config.setNetworkDevices(&NSArray::from_retained_slice(&[Retained::into_super(net)]));

        // --- Filesystem: virtio-fs rootfs share ---
        let fs_device = VZVirtioFileSystemDeviceConfiguration::initWithTag(
            VZVirtioFileSystemDeviceConfiguration::alloc(),
            &NSString::from_str("rootfs"),
        );
        let shared_dir = VZSharedDirectory::initWithURL_readOnly(
            VZSharedDirectory::alloc(),
            &path_to_ns_url(&config.rootfs_path),
            false,
        );
        let single_share =
            VZSingleDirectoryShare::initWithDirectory(VZSingleDirectoryShare::alloc(), &shared_dir);
        fs_device.setShare(Some(&Retained::into_super(single_share)));
        vz_config.setDirectorySharingDevices(&NSArray::from_retained_slice(&[
            Retained::into_super(fs_device),
        ]));

        // --- Serial Console ---
        let serial_ports = serial_config(config);
        vz_config.setSerialPorts(&NSArray::from_retained_slice(&serial_ports));

        // --- vsock: host ↔ guest exec channel ---
        let vsock = VZVirtioSocketDeviceConfiguration::new();
        vz_config.setSocketDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            vsock,
        )]));

        // --- Entropy (RNG) ---
        vz_config.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )]));

        // --- Memory Balloon ---
        vz_config.setMemoryBalloonDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new(),
        )]));

        vz_config
    }
}

fn boot_loader(config: &VmConfig) -> Retained<VZLinuxBootLoader> {
    unsafe {
        let kernel_url = path_to_ns_url(&config.kernel_path);
        let loader = VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url);
        // With initrd: k3rs-init boots from initrd, mounts virtiofs at /mnt/rootfs.
        // Without initrd: kernel mounts virtiofs directly as root (requires CONFIG_VIRTIO_FS=y).
        let cmdline = if config.initrd_path.is_some() {
            "console=hvc0 rdinit=/sbin/init rw loglevel=7"
        } else {
            "console=hvc0 root=virtiofs:rootfs rw rootwait init=/sbin/init loglevel=7"
        };
        loader.setCommandLine(&NSString::from_str(cmdline));
        if let Some(ref initrd) = config.initrd_path {
            loader.setInitialRamdiskURL(Some(&path_to_ns_url(initrd)));
        }
        loader
    }
}

fn serial_config(config: &VmConfig) -> Vec<Retained<VZSerialPortConfiguration>> {
    unsafe {
        let console = VZVirtioConsoleDeviceSerialPortConfiguration::new();

        if let Some(ref log_path) = config.log_path {
            // Ensure log file exists
            if !Path::new(log_path).exists() {
                let _ = File::create(log_path);
            }
            let log_url = path_to_ns_url(log_path);

            let attachment = VZFileSerialPortAttachment::initWithURL_append_error(
                VZFileSerialPortAttachment::alloc(),
                &log_url,
                true,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "failed to create serial port attachment, err={}",
                    err.localizedDescription()
                )
            });
            console.setAttachment(Some(&Retained::into_super(attachment)));
        } else {
            // If no log file, write to stdout
            let stdout_path_url = path_to_ns_url("/dev/stdout");

            let attachment = VZFileSerialPortAttachment::initWithURL_append_error(
                VZFileSerialPortAttachment::alloc(),
                &stdout_path_url,
                true,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "failed to create stdout serial port attachment, err={}",
                    err.localizedDescription()
                )
            });
            console.setAttachment(Some(&Retained::into_super(attachment)));
        }

        vec![Retained::into_super(console)]
    }
}

fn path_to_ns_url(path: &str) -> Retained<NSURL> {
    NSURL::initFileURLWithPath(NSURL::alloc(), &NSString::from_str(path))
}
