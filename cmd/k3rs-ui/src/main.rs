use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

mod api;
mod pages;

use pages::*;

// ============================================================
// Route definitions
// ============================================================
#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(Layout)]
        #[route("/")]
        Dashboard {},
        #[route("/nodes")]
        Nodes {},
        #[route("/workloads")]
        Workloads {},
        #[route("/services")]
        Services {},
        #[route("/ingress")]
        Ingress {},
        #[route("/events")]
        Events {},
}

// ============================================================
// Assets
// ============================================================
const FAVICON: Asset = asset!("/assets/favicon.ico");
const MAIN_CSS: Asset = asset!("/assets/main.css");

// ============================================================
// Entry point
// ============================================================
fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        Router::<Route> {}
    }
}

// ============================================================
// Layout — sidebar + main content area
// ============================================================
#[component]
fn Layout() -> Element {
    let mut namespace = use_signal(|| "default".to_string());
    let route: Route = use_route();

    let nav_items = vec![
        ("📊", "Dashboard", Route::Dashboard {}),
        ("🖥️", "Nodes", Route::Nodes {}),
        ("📦", "Workloads", Route::Workloads {}),
        ("🔗", "Services", Route::Services {}),
        ("🌐", "Ingress", Route::Ingress {}),
        ("📋", "Events", Route::Events {}),
    ];

    // Provide namespace as context for pages
    use_context_provider(move || namespace);

    rsx! {
        div { class: "app-layout",
            // Sidebar
            nav { class: "sidebar",
                div { class: "sidebar-brand",
                    h1 { "k3rs" }
                    div { class: "subtitle", "Management UI" }
                }

                // Namespace selector
                div { class: "sidebar-section",
                    div { class: "sidebar-section-title", "Namespace" }
                    select {
                        class: "ns-selector",
                        value: "{namespace}",
                        onchange: move |evt| {
                            namespace.set(evt.value());
                        },
                        option { value: "default", "default" }
                        option { value: "k3rs-system", "k3rs-system" }
                    }
                }

                // Nav links
                div { class: "sidebar-section",
                    div { class: "sidebar-section-title", "Navigation" }
                    for (icon, label, target) in nav_items {
                        Link {
                            class: if route == target { "sidebar-link active" } else { "sidebar-link" },
                            to: target,
                            span { class: "icon", "{icon}" }
                            span { "{label}" }
                        }
                    }
                }
            }

            // Main content
            main { class: "main-content",
                Outlet::<Route> {}
            }
        }
    }
}

// ============================================================
// Shared types (matching k3rs API types)
// ============================================================
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    pub registered_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Pod {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub status: String,
    #[serde(default)]
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: ServiceSpec,
    #[serde(default)]
    pub cluster_ip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServiceSpec {
    #[serde(default)]
    pub ports: Vec<ServicePort>,
    #[serde(default)]
    pub service_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServicePort {
    pub port: u16,
    pub target_port: u16,
    #[serde(default)]
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Deployment {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: DeploymentSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DeploymentSpec {
    pub replicas: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ConfigMap {
    pub id: String,
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub data: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Secret {
    pub id: String,
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub data: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IngressObj {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: IngressSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IngressSpec {
    #[serde(default)]
    pub rules: Vec<IngressRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IngressRule {
    pub host: String,
    pub http: IngressHTTP,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IngressHTTP {
    #[serde(default)]
    pub paths: Vec<IngressPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IngressPath {
    pub path: String,
    pub backend: IngressBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct IngressBackend {
    pub service_name: String,
    pub service_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEvent {
    pub seq: u64,
    pub event_type: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterInfo {
    pub endpoint: String,
    pub version: String,
    pub state_store: String,
    pub node_count: usize,
}
