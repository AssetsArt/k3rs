use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- VPC status ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VpcStatus {
    Active,
    Terminating,
    Deleted,
}

impl std::fmt::Display for VpcStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VpcStatus::Active => write!(f, "Active"),
            VpcStatus::Terminating => write!(f, "Terminating"),
            VpcStatus::Deleted => write!(f, "Deleted"),
        }
    }
}

// --- VPC ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vpc {
    pub name: String,
    pub vpc_id: u16,
    pub ipv4_cidr: String,
    pub status: VpcStatus,
    pub created_at: DateTime<Utc>,
    /// Set when status transitions to Deleted. VpcID cannot be reused until
    /// 300s after this timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
}

// --- VPC Peering ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PeeringDirection {
    Bidirectional,
    InitiatorOnly,
}

impl std::fmt::Display for PeeringDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeeringDirection::Bidirectional => write!(f, "Bidirectional"),
            PeeringDirection::InitiatorOnly => write!(f, "InitiatorOnly"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PeeringStatus {
    Active,
    Inactive,
}

impl std::fmt::Display for PeeringStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeeringStatus::Active => write!(f, "Active"),
            PeeringStatus::Inactive => write!(f, "Inactive"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpcPeering {
    pub name: String,
    pub vpc_a: String,
    pub vpc_b: String,
    pub direction: PeeringDirection,
    pub status: PeeringStatus,
    pub created_at: DateTime<Utc>,
}
