use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "k3rsctl", about = "CLI tool for k3rs cluster management")]
pub struct Cli {
    /// Server API endpoint
    #[arg(long, default_value = pkg_constants::network::DEFAULT_API_ADDR)]
    pub server: String,

    /// Authentication token
    #[arg(long, default_value = pkg_constants::auth::DEFAULT_JOIN_TOKEN)]
    pub token: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Show cluster information
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },
    /// Manage nodes
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Get resources
    Get {
        /// Resource type (pods, services, deployments, configmaps, secrets, namespaces, replicasets, daemonsets, jobs, cronjobs, hpa)
        resource: String,
        /// Namespace (default: "default")
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Describe a resource in detail
    Describe {
        /// Resource type (pod)
        resource: String,
        /// Resource name
        name: String,
        /// Namespace (default: "default")
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Apply a manifest file
    Apply {
        /// Path to YAML/JSON manifest
        #[arg(short, long)]
        file: String,
        /// Namespace (default: "default")
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Delete a resource (by type/id or from a manifest file)
    Delete {
        /// Resource type (e.g. pods, deployments)
        resource: Option<String>,
        /// Resource ID or name
        id: Option<String>,
        /// Path to YAML/JSON manifest to delete
        #[arg(short, long)]
        file: Option<String>,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Stream logs from a pod
    Logs {
        /// Pod ID
        pod_id: String,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
        /// Follow log output (poll every 2s)
        #[arg(short, long, default_value_t = false)]
        follow: bool,
    },
    /// Execute a command in a pod
    Exec {
        /// Pod ID
        pod_id: String,
        /// Command to execute (after --)
        #[arg(last = true)]
        command: Vec<String>,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
        /// Keep stdin open and allocate an interactive session
        #[arg(short = 'i', long = "it", default_value_t = false)]
        interactive: bool,
    },
    /// Manage container runtime
    Runtime {
        #[command(subcommand)]
        action: RuntimeAction,
    },
    /// Cluster backup management
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Local process manager (pm2-style)
    Pm {
        #[command(subcommand)]
        action: crate::pm::PmAction,
    },
    /// Diagnose cluster health and local environment
    Doctor,
    /// Restore cluster from a backup file
    Restore {
        /// Path to the `.k3rs-backup.json.gz` backup file
        #[arg(long, short)]
        from: String,
        /// Perform a dry-run validation without applying changes
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Skip confirmation prompt
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum ClusterAction {
    /// Display cluster info
    Info,
}

#[derive(Subcommand)]
pub enum NodeAction {
    /// List all registered nodes
    List,
    /// Drain a node (cordon + evict pods)
    Drain {
        /// Node name
        name: String,
    },
    /// Mark a node as unschedulable
    Cordon {
        /// Node name
        name: String,
    },
    /// Mark a node as schedulable again
    Uncordon {
        /// Node name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum RuntimeAction {
    /// Show current container runtime info
    Info,
    /// Upgrade/download the latest container runtime (Linux only)
    Upgrade,
}

#[derive(Subcommand)]
pub enum BackupAction {
    /// Create a backup and save it to a local file
    Create {
        /// Output path for the backup file (default: ./backup-<timestamp>.k3rs-backup.json.gz)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// List backup files in a directory
    List {
        /// Directory to list backups from (default: current directory)
        #[arg(short, long, default_value = ".")]
        dir: String,
    },
    /// Inspect metadata from a backup file
    Inspect {
        /// Path to the backup file
        file: String,
    },
    /// Show server-side backup status
    Status,
}
