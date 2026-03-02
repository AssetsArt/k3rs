use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Components managed by the process manager.
#[derive(Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ComponentName {
    Server,
    Agent,
    Vpc,
    Ui,
    All,
}

impl ComponentName {
    /// The binary name installed into `~/.k3rs/pm/bins/`.
    pub fn bin_name(&self) -> &str {
        match self {
            Self::Server => "k3rs-server",
            Self::Agent => "k3rs-agent",
            Self::Vpc => "k3rs-vpc",
            Self::Ui => "k3rs-ui",
            Self::All => unreachable!("All must be resolved before calling bin_name"),
        }
    }

    /// The Cargo package name (for `cargo build -p <pkg>`).
    pub fn cargo_package(&self) -> &str {
        match self {
            Self::Server => "k3rs-server",
            Self::Agent => "k3rs-agent",
            Self::Vpc => "k3rs-vpc",
            Self::Ui => "k3rs-ui",
            Self::All => unreachable!("All must be resolved before calling cargo_package"),
        }
    }

    /// Registry key for this component.
    pub fn key(&self) -> &str {
        match self {
            Self::Server => "server",
            Self::Agent => "agent",
            Self::Vpc => "vpc",
            Self::Ui => "ui",
            Self::All => unreachable!("All must be resolved before calling key"),
        }
    }

    /// Expand `All` into the individual components.
    pub fn resolve(&self) -> Vec<ComponentName> {
        match self {
            Self::All => vec![Self::Server, Self::Agent, Self::Vpc, Self::Ui],
            other => vec![other.clone()],
        }
    }
}

impl std::fmt::Display for ComponentName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.key())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Stopped,
    Crashed,
    Installing,
    Errored,
}

impl std::fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Crashed => write!(f, "crashed"),
            Self::Installing => write!(f, "installing"),
            Self::Errored => write!(f, "errored"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub name: String,
    pub bin_path: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub status: ProcessStatus,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub auto_restart: bool,
    pub max_restarts: u32,
    pub config_path: Option<PathBuf>,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PmRegistry {
    pub version: u32,
    pub processes: HashMap<String, ProcessEntry>,
}

impl Default for PmRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            processes: HashMap::new(),
        }
    }
}
