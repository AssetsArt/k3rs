import Foundation
import Virtualization

/// Manages the lifecycle of lightweight Linux microVMs using Apple's Virtualization.framework.
///
/// Each VM is configured with:
/// - VZLinuxBootLoader: boots a minimal Linux kernel
/// - virtio-fs: shares host rootfs directory as guest `/`
/// - virtio-net: NAT networking for pod connectivity
/// - virtio-console: serial console → log file
/// - virtio-vsock: host ↔ guest exec channel (port 5555)
final class VMManager: NSObject, VZVirtualMachineDelegate {

    /// Active VM instances keyed by container ID
    private var vms: [String: ManagedVM] = [:]
    private let queue = DispatchQueue(label: "io.k3rs.vmm", qos: .userInitiated)

    struct VMConfig {
        let id: String
        let kernelPath: String
        let initrdPath: String?
        let rootfsPath: String      // Directory path (not disk image)
        let cpuCount: Int
        let memoryMB: Int
        let logPath: String?
    }

    struct ManagedVM {
        let vm: VZVirtualMachine
        let config: VMConfig
        let logHandle: FileHandle?
        let vsockDevice: VZVirtioSocketDevice?
    }

    // MARK: - Boot

    func boot(config: VMConfig) throws {
        let vzConfig = try createVZConfiguration(config: config)
        try vzConfig.validate()

        let vm = VZVirtualMachine(configuration: vzConfig, queue: queue)
        vm.delegate = self

        // Open log file for console output
        var logHandle: FileHandle? = nil
        if let logPath = config.logPath {
            FileManager.default.createFile(atPath: logPath, contents: nil)
            logHandle = FileHandle(forWritingAtPath: logPath)
        }

        // Find vsock device (for exec)
        let vsockDevice = vm.socketDevices.first as? VZVirtioSocketDevice

        let managed = ManagedVM(
            vm: vm,
            config: config,
            logHandle: logHandle,
            vsockDevice: vsockDevice
        )
        vms[config.id] = managed

        let semaphore = DispatchSemaphore(value: 0)
        var bootError: Error? = nil

        queue.async {
            vm.start { result in
                switch result {
                case .success:
                    log("VM \(config.id) started successfully")
                case .failure(let error):
                    log("VM \(config.id) failed to start: \(error)")
                    bootError = error
                }
                semaphore.signal()
            }
        }

        let timeout = semaphore.wait(timeout: .now() + 30)
        if timeout == .timedOut {
            throw VMError.bootTimeout(config.id)
        }
        if let error = bootError {
            vms.removeValue(forKey: config.id)
            throw error
        }
    }

    // MARK: - Stop

    func stop(id: String) throws {
        guard let managed = vms[id] else {
            throw VMError.notFound(id)
        }

        let semaphore = DispatchSemaphore(value: 0)
        var stopError: Error? = nil

        queue.async {
            if managed.vm.canRequestStop {
                do {
                    try managed.vm.requestStop()
                    log("Requested graceful stop for VM \(id)")
                } catch {
                    log("Graceful stop failed, forcing: \(error)")
                }
            }

            // Force stop after brief wait
            DispatchQueue.global().asyncAfter(deadline: .now() + 3) {
                if managed.vm.state == .running || managed.vm.state == .paused {
                    managed.vm.stop { error in
                        if let error = error {
                            stopError = error
                        }
                        semaphore.signal()
                    }
                } else {
                    semaphore.signal()
                }
            }
        }

        let timeout = semaphore.wait(timeout: .now() + 10)
        managed.logHandle?.closeFile()
        vms.removeValue(forKey: id)

        if timeout == .timedOut {
            log("VM \(id) stop timed out — force killed")
        }
        if let error = stopError {
            throw error
        }
    }

    // MARK: - Exec via vsock

    func exec(id: String, command: [String]) throws -> String {
        guard let managed = vms[id] else {
            throw VMError.notFound(id)
        }
        guard let vsock = managed.vsockDevice else {
            throw VMError.noVsock(id)
        }
        guard managed.vm.state == .running else {
            throw VMError.notRunning(id)
        }

        let semaphore = DispatchSemaphore(value: 0)
        var result = ""
        var execError: Error? = nil

        // Connect to guest vsock port 5555 (k3rs-init exec listener)
        // The API uses Result<VZVirtioSocketConnection, Error> in the closure
        vsock.connect(toPort: 5555) { connectionResult in
            let conn: VZVirtioSocketConnection
            switch connectionResult {
            case .success(let c):
                conn = c
            case .failure(let error):
                execError = error
                semaphore.signal()
                return
            }

            // VZVirtioSocketConnection exposes a raw file descriptor
            let fd = conn.fileDescriptor
            let writeHandle = FileHandle(fileDescriptor: fd)
            let readHandle = FileHandle(fileDescriptor: fd)

            // Send command as NUL-delimited string + newline
            let cmdString = command.joined(separator: "\0") + "\n"
            if let data = cmdString.data(using: .utf8) {
                writeHandle.write(data)
            }

            // Read response (with a short delay to let the guest process)
            Thread.sleep(forTimeInterval: 0.1)
            let responseData = readHandle.availableData
            result = String(data: responseData, encoding: .utf8) ?? ""

            semaphore.signal()
        }

        let timeout = semaphore.wait(timeout: .now() + 30)
        if timeout == .timedOut {
            throw VMError.execTimeout(id)
        }
        if let execError = execError {
            throw execError
        }
        return result
    }

    // MARK: - State Query

    func state(id: String) -> String {
        guard let managed = vms[id] else { return "not_found" }
        switch managed.vm.state {
        case .stopped:  return "stopped"
        case .running:  return "running"
        case .paused:   return "paused"
        case .error:    return "error"
        case .starting: return "starting"
        case .pausing:  return "pausing"
        case .resuming: return "resuming"
        case .stopping: return "stopping"
        case .saving:   return "saving"
        case .restoring: return "restoring"
        @unknown default: return "unknown"
        }
    }

    // MARK: - VZ Configuration

    private func createVZConfiguration(config: VMConfig) throws -> VZVirtualMachineConfiguration {
        let vzConfig = VZVirtualMachineConfiguration()

        // --- Boot Loader ---
        let bootLoader = VZLinuxBootLoader(kernelURL: URL(fileURLWithPath: config.kernelPath))
        // Kernel command line: mount virtio-fs as root, use k3rs-init as init
        bootLoader.commandLine = "console=hvc0 root=virtiofs:rootfs rw init=/sbin/init"
        if let initrdPath = config.initrdPath {
            bootLoader.initialRamdiskURL = URL(fileURLWithPath: initrdPath)
        }
        vzConfig.bootLoader = bootLoader

        // --- CPU & Memory ---
        vzConfig.cpuCount = max(config.cpuCount, VZVirtualMachineConfiguration.minimumAllowedCPUCount)
        vzConfig.memorySize = UInt64(max(config.memoryMB, 64)) * 1024 * 1024

        // --- virtio-fs: Share rootfs directory ---
        let sharedDir = VZSharedDirectory(url: URL(fileURLWithPath: config.rootfsPath), readOnly: false)
        let singleDirShare = VZSingleDirectoryShare(directory: sharedDir)
        let fsConfig = VZVirtioFileSystemDeviceConfiguration(tag: "rootfs")
        fsConfig.share = singleDirShare
        vzConfig.directorySharingDevices = [fsConfig]

        // --- virtio-net: NAT networking ---
        let netConfig = VZVirtioNetworkDeviceConfiguration()
        netConfig.attachment = VZNATNetworkDeviceAttachment()
        vzConfig.networkDevices = [netConfig]

        // --- virtio-console: Serial console → log file ---
        let consoleConfig = VZVirtioConsoleDeviceSerialPortConfiguration()
        if let logPath = config.logPath {
            let logURL = URL(fileURLWithPath: logPath)
            // Create the file if needed
            if !FileManager.default.fileExists(atPath: logPath) {
                FileManager.default.createFile(atPath: logPath, contents: nil)
            }
            let logFileHandle = try FileHandle(forWritingTo: logURL)
            logFileHandle.seekToEndOfFile()
            consoleConfig.attachment = VZFileHandleSerialPortAttachment(
                fileHandleForReading: FileHandle.nullDevice,
                fileHandleForWriting: logFileHandle
            )
        }
        vzConfig.serialPorts = [consoleConfig]

        // --- virtio-vsock: Host ↔ Guest exec channel ---
        let vsockConfig = VZVirtioSocketDeviceConfiguration()
        vzConfig.socketDevices = [vsockConfig]

        // --- Entropy (RNG) ---
        vzConfig.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]

        // --- Memory Balloon ---
        vzConfig.memoryBalloonDevices = [VZVirtioTraditionalMemoryBalloonDeviceConfiguration()]

        return vzConfig
    }

    // MARK: - VZVirtualMachineDelegate

    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        let id = vms.first(where: { $0.value.vm === virtualMachine })?.key ?? "unknown"
        log("VM \(id) stopped with error: \(error)")
        vms.removeValue(forKey: id)
    }

    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        let id = vms.first(where: { $0.value.vm === virtualMachine })?.key ?? "unknown"
        log("VM \(id) guest stopped gracefully")
        vms.removeValue(forKey: id)
    }
}

// MARK: - Errors

enum VMError: Error, CustomStringConvertible {
    case notFound(String)
    case notRunning(String)
    case noVsock(String)
    case bootTimeout(String)
    case execTimeout(String)

    var description: String {
        switch self {
        case .notFound(let id):     return "VM '\(id)' not found"
        case .notRunning(let id):   return "VM '\(id)' is not running"
        case .noVsock(let id):      return "VM '\(id)' has no vsock device"
        case .bootTimeout(let id):  return "VM '\(id)' boot timed out"
        case .execTimeout(let id):  return "VM '\(id)' exec timed out"
        }
    }
}

// MARK: - Logging

func log(_ message: String) {
    let timestamp = ISO8601DateFormatter().string(from: Date())
    FileHandle.standardError.write("[\(timestamp)] [k3rs-vmm] \(message)\n".data(using: .utf8)!)
}
