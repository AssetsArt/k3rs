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
        subcommands: [Boot.self, Stop.self, Exec.self, State.self],
        defaultSubcommand: Boot.self
    )

    /// Track current VM ID for signal handler
    static var currentVMId: String?

    /// Global VM manager (process-lifetime singleton).
    static let vmManager = VMManager()

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
        let output = try K3rsVMM.vmManager.exec(id: id, command: command)
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
        let vmState = K3rsVMM.vmManager.state(id: id)
        print("state=\(vmState)")
    }
}

// Entry point
K3rsVMM.main()
