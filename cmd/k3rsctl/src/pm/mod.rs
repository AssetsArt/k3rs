pub mod dev;
pub mod install;
pub mod lifecycle;
pub mod list;
pub mod logs;
pub mod registry;
pub mod startup;
pub mod status;
pub mod tui;
pub mod types;
pub mod watchdog;

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

use types::ComponentName;

#[derive(Subcommand)]
pub enum PmAction {
    /// Install a component binary (build from source or copy existing)
    Install {
        /// Component to install
        component: ComponentName,
        /// Build from the local Cargo workspace
        #[arg(long)]
        from_source: bool,
        /// Path to an existing binary to register
        #[arg(long)]
        bin_path: Option<PathBuf>,
    },
    /// Start a component as a background daemon
    Start {
        /// Component to start
        component: ComponentName,
        /// Run in foreground instead of daemonizing
        #[arg(long)]
        foreground: bool,
    },
    /// Stop a running component
    Stop {
        /// Component to stop
        component: ComponentName,
        /// Send SIGKILL immediately instead of SIGTERM
        #[arg(long)]
        force: bool,
        /// Seconds to wait before escalating to SIGKILL
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
    /// Restart a component (stop + start)
    Restart {
        /// Component to restart
        component: ComponentName,
        /// Send SIGKILL immediately during stop
        #[arg(long)]
        force: bool,
        /// Seconds to wait before escalating to SIGKILL
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
    /// List all managed components with status
    List,
    /// Remove a component from PM management
    Delete {
        /// Component to delete
        component: ComponentName,
        /// Don't delete data directory
        #[arg(long)]
        keep_data: bool,
        /// Don't delete binary
        #[arg(long)]
        keep_binary: bool,
        /// Don't delete logs
        #[arg(long)]
        keep_logs: bool,
    },
    /// Generate systemd unit files for all registered components
    Startup {
        /// Generate user-level units (~/.config/systemd/user/)
        #[arg(long)]
        user: bool,
        /// Run systemctl enable after generation
        #[arg(long)]
        enable: bool,
    },
    /// Detailed status of all components with health checks
    Status,
    /// Internal: watchdog supervisor sidecar (not user-facing)
    #[command(hide = true)]
    #[allow(non_camel_case_types)]
    _Watch {
        /// Component to watch
        component: ComponentName,
    },
    /// Run components in dev mode with auto-rebuild on code changes
    Dev {
        /// Component(s) to run in dev mode (server, agent, vpc, ui, or all)
        component: ComponentName,
    },
    /// Tail or stream component logs
    Logs {
        /// Component to view logs for
        component: ComponentName,
        /// Stream logs continuously
        #[arg(long, short)]
        follow: bool,
        /// Number of lines to show (default: 50)
        #[arg(long, default_value_t = 50)]
        lines: usize,
        /// Show stderr log only
        #[arg(long)]
        error: bool,
    },
}

/// Dispatch a PM subcommand.
pub async fn handle(action: &PmAction) -> Result<()> {
    match action {
        PmAction::Install {
            component,
            from_source,
            bin_path,
        } => install::install(
            component,
            *from_source,
            bin_path.as_deref().map(|p| p.to_str().unwrap()),
        ),
        PmAction::Start {
            component,
            foreground,
        } => lifecycle::start(component, *foreground),
        PmAction::Stop {
            component,
            force,
            timeout,
        } => lifecycle::stop(component, *force, *timeout),
        PmAction::Restart {
            component,
            force,
            timeout,
        } => lifecycle::restart(component, *force, *timeout),
        PmAction::List => list::list(),
        PmAction::Delete {
            component,
            keep_data,
            keep_binary,
            keep_logs,
        } => lifecycle::delete(component, *keep_data, *keep_binary, *keep_logs),
        PmAction::Status => status::status(),
        PmAction::Startup { user, enable } => startup::startup(*user, *enable),
        PmAction::_Watch { component } => watchdog::run(component),
        PmAction::Dev { component } => dev::run(component),
        PmAction::Logs {
            component,
            follow,
            lines,
            error,
        } => logs::logs(component, *follow, *lines, *error),
    }
}
