use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigMap {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub data: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
}
