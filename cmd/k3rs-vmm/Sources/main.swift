import ArgumentParser
import Foundation

/// k3rs-vmm — Host-side VMM helper for k3rs.
///
/// Wraps Apple's Virtualization.framework to boot lightweight Linux microVMs
/// for container pod isolation on macOS.
struct K3rsVMM: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "k3rs-vmm",
        abstract: "k3rs Virtual Machine Monitor — boots Linux microVMs via Virtualization.framework",
        subcommands: [Boot.self, Stop.self, Exec.self, State.self, List.self, Remove.self],
        defaultSubcommand: Boot.self
    )

    /// Track current VM ID for signal handler
    static var currentVMId: String?

    /// Lazy-loaded VM manager — only created when Boot/Stop actually need it.
    /// This avoids loading Virtualization.framework for ls/rm/exec commands.
    private static var _vmManager: VMManager?
    static var vmManager: VMManager {
        if _vmManager == nil {
            _vmManager = VMManager()
        }
        return _vmManager!
    }

    static func log(_ message: String) {
        let ts = ISO8601DateFormatter().string(from: Date())
        FileHandle.standardError.write("[\(ts)] [k3rs-vmm] \(message)\n".data(using: .utf8)!)
    }
}

// MARK: - Boot Subcommand

struct Boot: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Boot a Linux microVM"
    )

    @Option(name: .long, help: "Path to Linux kernel (vmlinux)")
    var kernel: String

    @Option(name: .long, help: "Path to initial ramdisk (initrd.img)")
    var initrd: String?

    @Option(name: .long, help: "Path to rootfs directory (shared via virtio-fs)")
    var rootfs: String

    @Option(name: .long, help: "Number of vCPUs")
    var cpus: Int = 1

    @Option(name: .long, help: "Memory in MB")
    var memory: Int = 128

    @Option(name: .long, help: "Container/VM ID")
    var id: String

    @Option(name: .long, help: "Path to log file for console output")
    var log: String?

    @Flag(name: .long, help: "Run in foreground (block until VM exits)")
    var foreground: Bool = false

    func run() throws {
        K3rsVMM.log("Booting VM: id=\(id), kernel=\(kernel), rootfs=\(rootfs), cpus=\(cpus), memory=\(memory)MB")

        let config = VMManager.VMConfig(
            id: id,
            kernelPath: kernel,
            initrdPath: initrd,
            rootfsPath: rootfs,
            cpuCount: cpus,
            memoryMB: memory,
            logPath: log
        )

        try K3rsVMM.vmManager.boot(config: config)

        if foreground {
            K3rsVMM.log("VM \(id) running in foreground — press Ctrl+C to stop")
            K3rsVMM.currentVMId = id

            signal(SIGINT) { _ in
                K3rsVMM.log("Received SIGINT — stopping VM")
                try? K3rsVMM.vmManager.stop(id: K3rsVMM.currentVMId ?? "")
                Foundation.exit(0)
            }
            signal(SIGTERM) { _ in
                K3rsVMM.log("Received SIGTERM — stopping VM")
                try? K3rsVMM.vmManager.stop(id: K3rsVMM.currentVMId ?? "")
                Foundation.exit(0)
            }

            RunLoop.main.run()
        } else {
            // Print PID and exit (for daemon mode - parent process tracks us)
            print("pid=\(ProcessInfo.processInfo.processIdentifier)")
            print("state=running")
        }
    }
}

// MARK: - Stop Subcommand

struct Stop: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Stop a running microVM"
    )

    @Option(name: .long, help: "Container/VM ID")
    var id: String

    func run() throws {
        K3rsVMM.log("Stopping VM: \(id)")
        try K3rsVMM.vmManager.stop(id: id)
        print("state=stopped")
    }
}

// MARK: - Exec Subcommand

struct Exec: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Execute a command inside a running microVM via vsock"
    )

    @Option(name: .long, help: "Container/VM ID")
    var id: String

    @Argument(parsing: .captureForPassthrough, help: "Command to execute in guest")
    var command: [String]

    func run() throws {
        guard !command.isEmpty else {
            K3rsVMM.log("No command specified for exec")
            throw ExitCode.failure
        }

        K3rsVMM.log("Exec in VM \(id): \(command.joined(separator: " "))")

        // Connect to the running boot process via Unix domain socket IPC
        let output = try VMManager.execViaIPC(id: id, command: command)
        print(output, terminator: "")
    }
}

// MARK: - State Subcommand

struct State: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Query the state of a microVM"
    )

    @Option(name: .long, help: "Container/VM ID")
    var id: String

    func run() {
        // Check IPC socket first (running in another process)
        let sockPath = VMManager.socketPath(for: id)
        if FileManager.default.fileExists(atPath: sockPath) {
            print("state=running")
            return
        }
        print("state=not_found")
    }
}

// MARK: - List Subcommand

struct List: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "ls",
        abstract: "List running microVMs"
    )

    func run() {
        let fm = FileManager.default
        let vmsDir = "/tmp/k3rs-runtime/vms"

        // Find active VMs by scanning IPC socket files
        guard let contents = try? fm.contentsOfDirectory(atPath: vmsDir) else {
            print("No VMs found")
            return
        }

        let sockets = contents
            .filter { $0.hasPrefix("vmm-") && $0.hasSuffix(".sock") }
            .sorted()

        if sockets.isEmpty {
            print("No VMs running")
            return
        }

        print("\("VM ID".padding(toLength: 38, withPad: " ", startingAt: 0))  \("PID".padding(toLength: 8, withPad: " ", startingAt: 0))  STATE")
        print(String(repeating: "-", count: 60))

        for sockFile in sockets {
            let id = sockFile
                .replacingOccurrences(of: "vmm-", with: "")
                .replacingOccurrences(of: ".sock", with: "")

            var pid = "-"
            var state = "running"

            // Find PID via pgrep
            let pipe = Pipe()
            let proc = Process()
            proc.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
            proc.arguments = ["-f", "k3rs-vmm boot.*--id \(id)"]
            proc.standardOutput = pipe
            proc.standardError = FileHandle.nullDevice
            try? proc.run()
            proc.waitUntilExit()

            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let pidStr = String(data: data, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

            if let firstPid = pidStr.components(separatedBy: "\n").first, !firstPid.isEmpty {
                pid = firstPid
            } else {
                state = "stale"
                // Clean up stale socket
                try? fm.removeItem(atPath: "\(vmsDir)/\(sockFile)")
            }

            print("\(id.padding(toLength: 38, withPad: " ", startingAt: 0))  \(pid.padding(toLength: 8, withPad: " ", startingAt: 0))  \(state)")
        }
    }
}

// MARK: - Remove Subcommand

struct Remove: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "rm",
        abstract: "Remove (kill) a running microVM"
    )

    @Argument(help: "VM ID to remove")
    var id: String

    @Flag(name: .shortAndLong, help: "Force kill (SIGKILL instead of SIGTERM)")
    var force: Bool = false

    func run() throws {
        let sockPath = VMManager.socketPath(for: id)

        // Find PID of the boot process
        let pipe = Pipe()
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
        proc.arguments = ["-f", "k3rs-vmm boot.*--id \(id)"]
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        try proc.run()
        proc.waitUntilExit()

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let pidStr = String(data: data, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        if let firstPid = pidStr.components(separatedBy: "\n").first,
           let pidInt = Int32(firstPid) {
            if force {
                kill(pidInt, SIGKILL)
                print("VM \(id) force killed (pid=\(pidInt))")
            } else {
                kill(pidInt, SIGTERM)
                print("VM \(id) stopped (pid=\(pidInt))")

                // Wait briefly for graceful shutdown
                Thread.sleep(forTimeInterval: 1)

                // Check if still running
                if kill(pidInt, 0) == 0 {
                    kill(pidInt, SIGKILL)
                    print("VM \(id) force killed after timeout")
                }
            }
        } else {
            print("VM \(id) process not found")
        }

        // Clean up socket file
        try? FileManager.default.removeItem(atPath: sockPath)
        print("Cleaned up \(sockPath)")
    }
}

// Entry point
K3rsVMM.main()
