use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secret {
    pub id: String,
    pub name: String,
    pub namespace: String,
    /// Secret data stored as base64-encoded values.
    pub data: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
}
