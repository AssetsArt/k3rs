use serde::{Deserialize, Serialize};

/// Server configuration file (YAML).
///
/// Example `config.yaml`:
/// ```yaml
/// port: 6443
/// data-dir: /var/lib/k3rs/data
/// token: my-secret-token
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfigFile {
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default, alias = "data-dir")]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default, alias = "node-name")]
    pub node_name: Option<String>,
}

/// Agent configuration file (YAML).
///
/// Example `config.yaml`:
/// ```yaml
/// server: https://10.0.0.1:6443
/// token: my-secret-token
/// node-name: worker-1
/// proxy-port: 6444
/// service-proxy-port: 10256
/// dns-port: 5353
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfigFile {
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default, alias = "node-name")]
    pub node_name: Option<String>,
    #[serde(default, alias = "proxy-port")]
    pub proxy_port: Option<u16>,
    #[serde(default, alias = "service-proxy-port")]
    pub service_proxy_port: Option<u16>,
    #[serde(default, alias = "dns-port")]
    pub dns_port: Option<u16>,
}

/// Load a YAML config file, returning the default if the file doesn't exist.
pub fn load_config_file<T: serde::de::DeserializeOwned + Default>(path: &str) -> anyhow::Result<T> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(T::default());
        }
        Err(e) => return Err(e.into()),
    };
    let config: T = serde_yaml::from_str(&content)?;
    Ok(config)
}
