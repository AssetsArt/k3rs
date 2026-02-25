use serde::{Deserialize, Serialize};

// --- Policy rules ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// API groups this rule applies to (e.g., "" for core, "*" for all)
    pub api_groups: Vec<String>,
    /// Resource types (e.g., "pods", "services", "*" for all)
    pub resources: Vec<String>,
    /// Allowed verbs (e.g., "get", "list", "create", "update", "delete", "*" for all)
    pub verbs: Vec<String>,
}

// --- Role ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub namespace: String,
    pub rules: Vec<PolicyRule>,
}

// --- Subject ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubjectKind {
    User,
    ServiceAccount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub kind: SubjectKind,
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
}

// --- RoleBinding ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleBinding {
    pub name: String,
    pub namespace: String,
    pub role_ref: String,
    pub subjects: Vec<Subject>,
}
