use serde::Deserialize;

/// Minimal OCI runtime spec — only the fields we need.
#[derive(Debug, Deserialize)]
pub struct OciSpec {
    #[serde(default)]
    pub process: Option<OciProcess>,
    #[serde(default)]
    pub hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OciProcess {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}
