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

    /// Active IPC listeners keyed by container ID
    private var ipcListeners: [String: IPCListener] = [:]

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

    // MARK: - IPC Listener

    /// Unix domain socket listener for receiving exec requests from other k3rs-vmm processes.
    class IPCListener {
        let socketPath: String
        let socketFd: Int32
        var isRunning = true

        init(socketPath: String) throws {
            self.socketPath = socketPath

            // Remove stale socket
            unlink(socketPath)

            // Create Unix domain socket
            socketFd = socket(AF_UNIX, SOCK_STREAM, 0)
            guard socketFd >= 0 else {
                throw VMError.ipcError("Failed to create socket: \(String(cString: strerror(errno)))")
            }

            var addr = sockaddr_un()
            addr.sun_family = sa_family_t(AF_UNIX)
            let pathBytes = socketPath.utf8CString
            withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
                let raw = UnsafeMutableRawPointer(ptr)
                pathBytes.withUnsafeBufferPointer { buf in
                    raw.copyMemory(from: buf.baseAddress!, byteCount: min(buf.count, 104))
                }
            }

            let bindResult = withUnsafePointer(to: &addr) { ptr in
                ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                    Darwin.bind(socketFd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            guard bindResult == 0 else {
                Darwin.close(socketFd)
                throw VMError.ipcError("Failed to bind: \(String(cString: strerror(errno)))")
            }

            guard listen(socketFd, 5) == 0 else {
                Darwin.close(socketFd)
                throw VMError.ipcError("Failed to listen: \(String(cString: strerror(errno)))")
            }
        }

        func accept() -> Int32? {
            var clientAddr = sockaddr_un()
            var addrLen = socklen_t(MemoryLayout<sockaddr_un>.size)
            let clientFd = withUnsafeMutablePointer(to: &clientAddr) { ptr in
                ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                    Darwin.accept(socketFd, sockPtr, &addrLen)
                }
            }
            return clientFd >= 0 ? clientFd : nil
        }

        func close() {
            isRunning = false
            Darwin.close(socketFd)
            unlink(socketPath)
        }

        deinit {
            close()
        }
    }

    /// Socket path for a given VM ID
    static func socketPath(for id: String) -> String {
        return "/tmp/k3rs-vmm-\(id).sock"
    }

    /// Start IPC listener for exec requests targeting a specific VM.
    func startIPCListener(id: String) {
        let path = VMManager.socketPath(for: id)
        do {
            let listener = try IPCListener(socketPath: path)
            ipcListeners[id] = listener
            log("IPC listener started on \(path)")

            // Accept connections in background
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                while listener.isRunning {
                    guard let clientFd = listener.accept() else {
                        if listener.isRunning {
                            Thread.sleep(forTimeInterval: 0.01)
                        }
                        continue
                    }
                    self?.handleIPCClient(id: id, clientFd: clientFd)
                }
            }
        } catch {
            log("Failed to start IPC listener: \(error)")
        }
    }

    /// Handle an exec request from a client connected to the IPC socket.
    private func handleIPCClient(id: String, clientFd: Int32) {
        defer { Darwin.close(clientFd) }

        let readHandle = FileHandle(fileDescriptor: clientFd, closeOnDealloc: false)
        let writeHandle = FileHandle(fileDescriptor: clientFd, closeOnDealloc: false)

        // Read command (format: "cmd\0arg1\0arg2\n")
        let data = readHandle.availableData
        guard !data.isEmpty, let cmdString = String(data: data, encoding: .utf8) else {
            return
        }

        let cmdLine = cmdString.trimmingCharacters(in: .whitespacesAndNewlines)
        let parts = cmdLine.split(separator: "\0").map(String.init)
        guard !parts.isEmpty else { return }

        log("IPC exec request for VM \(id): \(parts)")

        // Execute via vsock to guest
        do {
            let output = try exec(id: id, command: parts)
            if let outputData = output.data(using: .utf8) {
                writeHandle.write(outputData)
            }
        } catch {
            let errMsg = "exec error: \(error)\n"
            if let errData = errMsg.data(using: .utf8) {
                writeHandle.write(errData)
            }
        }
    }

    /// Stop IPC listener for a VM.
    func stopIPCListener(id: String) {
        ipcListeners[id]?.close()
        ipcListeners.removeValue(forKey: id)
    }

    // MARK: - IPC Client (for exec subcommand)

    /// Connect to a running boot process's IPC socket and send an exec request.
    static func execViaIPC(id: String, command: [String]) throws -> String {
        let path = socketPath(for: id)

        // Check socket exists
        guard FileManager.default.fileExists(atPath: path) else {
            throw VMError.notFound(id)
        }

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw VMError.ipcError("Failed to create socket")
        }
        defer { Darwin.close(fd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = path.utf8CString
        withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
            let raw = UnsafeMutableRawPointer(ptr)
            pathBytes.withUnsafeBufferPointer { buf in
                raw.copyMemory(from: buf.baseAddress!, byteCount: min(buf.count, 104))
            }
        }

        let connectResult = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                Darwin.connect(fd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connectResult == 0 else {
            throw VMError.ipcError("Failed to connect to IPC socket at \(path): \(String(cString: strerror(errno)))")
        }

        // Send command as NUL-delimited string + newline
        let cmdString = command.joined(separator: "\0") + "\n"
        if let data = cmdString.data(using: .utf8) {
            let writeHandle = FileHandle(fileDescriptor: fd, closeOnDealloc: false)
            writeHandle.write(data)

            // Signal that we're done writing
            shutdown(fd, SHUT_WR)
        }

        // Read response
        let readHandle = FileHandle(fileDescriptor: fd, closeOnDealloc: false)
        var responseData = Data()
        // Read until EOF
        while true {
            let chunk = readHandle.availableData
            if chunk.isEmpty { break }
            responseData.append(chunk)
        }

        return String(data: responseData, encoding: .utf8) ?? ""
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

        // Start IPC listener so exec subcommand can reach this VM
        startIPCListener(id: config.id)
    }

    // MARK: - Stop

    func stop(id: String) throws {
        guard let managed = vms[id] else {
            throw VMError.notFound(id)
        }

        // Stop IPC listener first
        stopIPCListener(id: id)

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
        stopIPCListener(id: id)
        vms.removeValue(forKey: id)
    }

    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        let id = vms.first(where: { $0.value.vm === virtualMachine })?.key ?? "unknown"
        log("VM \(id) guest stopped gracefully")
        stopIPCListener(id: id)
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
    case ipcError(String)

    var description: String {
        switch self {
        case .notFound(let id):     return "VM '\(id)' not found"
        case .notRunning(let id):   return "VM '\(id)' is not running"
        case .noVsock(let id):      return "VM '\(id)' has no vsock device"
        case .bootTimeout(let id):  return "VM '\(id)' boot timed out"
        case .execTimeout(let id):  return "VM '\(id)' exec timed out"
        case .ipcError(let msg):    return "IPC error: \(msg)"
        }
    }
}

// MARK: - Logging

func log(_ message: String) {
    let timestamp = ISO8601DateFormatter().string(from: Date())
    FileHandle.standardError.write("[\(timestamp)] [k3rs-vmm] \(message)\n".data(using: .utf8)!)
}
