use dioxus::prelude::*;
use dioxus_free_icons::icons::ld_icons::*;
use dioxus_free_icons::Icon;
use serde::{Deserialize, Serialize};

mod api;
mod pages;

use pages::*;

// ============================================================
// Routes
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
const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        Router::<Route> {}
    }
}

// ============================================================
// Layout
// ============================================================
#[component]
fn Layout() -> Element {
    let mut namespace = use_signal(|| "default".to_string());
    let route: Route = use_route();
    use_context_provider(move || namespace);

    let link_cls = |target: &Route| {
        if *target == route {
            "flex items-center gap-2.5 px-3 py-2 rounded-lg text-sm font-medium text-blue-400 bg-blue-500/10"
        } else {
            "flex items-center gap-2.5 px-3 py-2 rounded-lg text-sm font-medium text-slate-400 hover:text-slate-200 hover:bg-slate-800/60 transition-all"
        }
    };

    rsx! {
        div { class: "flex min-h-screen",
            // Sidebar
            nav { class: "w-56 bg-slate-900 border-r border-slate-800 fixed top-0 left-0 bottom-0 flex flex-col",
                div { class: "px-5 py-5 border-b border-slate-800",
                    h1 { class: "text-lg font-bold text-white tracking-tight", "k3rs" }
                    p { class: "text-[10px] text-slate-500 uppercase tracking-widest mt-0.5", "management" }
                }

                div { class: "px-3 py-3",
                    p { class: "text-[10px] text-slate-500 uppercase tracking-widest px-2 mb-1.5", "Namespace" }
                    select {
                        class: "w-full bg-slate-800 border border-slate-700 rounded-md px-2.5 py-1.5 text-xs text-slate-300 outline-none focus:border-blue-500 transition-colors",
                        value: "{namespace}",
                        onchange: move |evt| namespace.set(evt.value()),
                        option { value: "default", "default" }
                        option { value: "k3rs-system", "k3rs-system" }
                    }
                }

                div { class: "flex-1 px-3 py-1 space-y-0.5",
                    p { class: "text-[10px] text-slate-500 uppercase tracking-widest px-2 mb-1.5", "Menu" }
                    Link { class: link_cls(&Route::Dashboard {}), to: Route::Dashboard {},
                        Icon { width: 16, height: 16, icon: LdLayoutDashboard }
                        span { "Dashboard" }
                    }
                    Link { class: link_cls(&Route::Nodes {}), to: Route::Nodes {},
                        Icon { width: 16, height: 16, icon: LdServer }
                        span { "Nodes" }
                    }
                    Link { class: link_cls(&Route::Workloads {}), to: Route::Workloads {},
                        Icon { width: 16, height: 16, icon: LdBox }
                        span { "Workloads" }
                    }
                    Link { class: link_cls(&Route::Services {}), to: Route::Services {},
                        Icon { width: 16, height: 16, icon: LdNetwork }
                        span { "Services" }
                    }
                    Link { class: link_cls(&Route::Ingress {}), to: Route::Ingress {},
                        Icon { width: 16, height: 16, icon: LdGlobe }
                        span { "Ingress" }
                    }
                    Link { class: link_cls(&Route::Events {}), to: Route::Events {},
                        Icon { width: 16, height: 16, icon: LdActivity }
                        span { "Events" }
                    }
                }
            }

            // Main
            main { class: "ml-56 flex-1 p-8 min-h-screen",
                Outlet::<Route> {}
            }
        }
    }
}

// ============================================================
// Shared types
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
