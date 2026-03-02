pub mod types;
pub mod registry;
pub mod install;
pub mod lifecycle;
pub mod list;

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
}

/// Dispatch a PM subcommand.
pub async fn handle(action: &PmAction) -> Result<()> {
    match action {
        PmAction::Install {
            component,
            from_source,
            bin_path,
        } => install::install(component, *from_source, bin_path.as_deref().map(|p| p.to_str().unwrap())),
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
    }
}
