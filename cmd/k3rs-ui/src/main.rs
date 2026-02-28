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
        #[route("/deployments")]
        Deployments {},
        #[route("/services")]
        Services {},
        #[route("/pods")]
        Pods {},
        #[route("/configmaps")]
        ConfigMaps {},
        #[route("/secrets")]
        Secrets {},
        #[route("/ingress")]
        Ingress {},
        #[route("/events")]
        Events {},
        #[route("/quotas")]
        Quotas {},
        #[route("/network-policies")]
        NetworkPolicies {},
        #[route("/volumes")]
        Volumes {},
        #[route("/processes")]
        ProcessList {},
        #[route("/images")]
        Images {},
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

    let nav_cls = |target: &Route| {
        if *target == route {
            "flex items-center gap-2.5 px-3 py-2 rounded-lg text-sm font-medium text-blue-400 bg-blue-500/10 ring-1 ring-blue-500/20"
        } else {
            "flex items-center gap-2.5 px-3 py-2 rounded-lg text-sm font-medium text-slate-400 hover:text-slate-200 hover:bg-white/5 transition-all"
        }
    };
    let sub_cls = |target: &Route| {
        if *target == route {
            "flex items-center gap-2.5 px-3 py-1.5 rounded-lg text-[13px] font-medium text-blue-400 bg-blue-500/10 ring-1 ring-blue-500/20"
        } else {
            "flex items-center gap-2.5 px-3 py-1.5 rounded-lg text-[13px] font-medium text-slate-500 hover:text-slate-300 hover:bg-white/5 transition-all"
        }
    };

    rsx! {
        div { class: "flex min-h-screen bg-slate-950",
            // ── Sidebar ──────────────────────────────────────────
            nav { class: "w-56 bg-slate-950 border-r border-slate-800/60 fixed top-0 left-0 bottom-0 flex flex-col",

                // Brand
                div { class: "px-4 py-5 border-b border-slate-800/60",
                    div { class: "flex items-center gap-2.5",
                        div { class: "w-8 h-8 rounded-xl bg-gradient-to-br from-blue-500 to-violet-600 flex items-center justify-center shrink-0 shadow-lg shadow-blue-500/20",
                            span { class: "text-white text-xs font-black tracking-tight", "k3" }
                        }
                        div {
                            p { class: "text-sm font-bold text-white leading-tight", "k3rs" }
                            p { class: "text-[9px] font-medium text-slate-500 uppercase tracking-widest", "cluster" }
                        }
                    }
                }

                // Namespace selector
                div { class: "px-3 py-3 border-b border-slate-800/60",
                    label { class: "text-[9px] font-semibold uppercase tracking-widest text-slate-600 px-1 mb-1.5 block",
                        "Namespace"
                    }
                    div { class: "flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-slate-900 border border-slate-700/60 hover:border-slate-600/60 transition-colors",
                        div { class: "w-1.5 h-1.5 rounded-full bg-emerald-400 shrink-0 shadow-sm shadow-emerald-400/50" }
                        select {
                            class: "flex-1 bg-transparent text-xs text-slate-300 outline-none cursor-pointer",
                            value: "{namespace}",
                            onchange: move |evt| namespace.set(evt.value()),
                            option { value: "default", "default" }
                            option { value: "k3rs-system", "k3rs-system" }
                        }
                    }
                }

                // Navigation
                div { class: "flex-1 px-2 py-3 overflow-y-auto space-y-0.5",

                    p { class: "text-[9px] font-bold uppercase tracking-[0.15em] text-slate-600 px-3 py-1 mt-0.5",
                        "Overview"
                    }
                    Link { class: nav_cls(&Route::Dashboard {}), to: Route::Dashboard {},
                        Icon { width: 14, height: 14, icon: LdLayoutDashboard }
                        "Dashboard"
                    }
                    Link { class: nav_cls(&Route::Nodes {}), to: Route::Nodes {},
                        Icon { width: 14, height: 14, icon: LdServer }
                        "Nodes"
                    }

                    p { class: "text-[9px] font-bold uppercase tracking-[0.15em] text-slate-600 px-3 py-1 mt-3",
                        "Workloads"
                    }
                    Link { class: sub_cls(&Route::Deployments {}), to: Route::Deployments {},
                        Icon { width: 13, height: 13, icon: LdRocket }
                        "Deployments"
                    }
                    Link { class: sub_cls(&Route::Pods {}), to: Route::Pods {},
                        Icon { width: 13, height: 13, icon: LdBox }
                        "Pods"
                    }

                    p { class: "text-[9px] font-bold uppercase tracking-[0.15em] text-slate-600 px-3 py-1 mt-3",
                        "Config"
                    }
                    Link { class: sub_cls(&Route::ConfigMaps {}), to: Route::ConfigMaps {},
                        Icon { width: 13, height: 13, icon: LdFileText }
                        "ConfigMaps"
                    }
                    Link { class: sub_cls(&Route::Secrets {}), to: Route::Secrets {},
                        Icon { width: 13, height: 13, icon: LdKeyRound }
                        "Secrets"
                    }

                    p { class: "text-[9px] font-bold uppercase tracking-[0.15em] text-slate-600 px-3 py-1 mt-3",
                        "Networking"
                    }
                    Link { class: sub_cls(&Route::Services {}), to: Route::Services {},
                        Icon { width: 13, height: 13, icon: LdNetwork }
                        "Services"
                    }
                    Link { class: sub_cls(&Route::Ingress {}), to: Route::Ingress {},
                        Icon { width: 13, height: 13, icon: LdGlobe }
                        "Ingress"
                    }
                    Link { class: sub_cls(&Route::NetworkPolicies {}), to: Route::NetworkPolicies {},
                        Icon { width: 13, height: 13, icon: LdShield }
                        "Network Policies"
                    }

                    p { class: "text-[9px] font-bold uppercase tracking-[0.15em] text-slate-600 px-3 py-1 mt-3",
                        "Storage"
                    }
                    Link { class: sub_cls(&Route::Volumes {}), to: Route::Volumes {},
                        Icon { width: 13, height: 13, icon: LdHardDrive }
                        "Volumes"
                    }
                    Link { class: sub_cls(&Route::Quotas {}), to: Route::Quotas {},
                        Icon { width: 13, height: 13, icon: LdGauge }
                        "Resource Quotas"
                    }

                    p { class: "text-[9px] font-bold uppercase tracking-[0.15em] text-slate-600 px-3 py-1 mt-3",
                        "Cluster"
                    }
                    Link { class: sub_cls(&Route::ProcessList {}), to: Route::ProcessList {},
                        Icon { width: 13, height: 13, icon: LdCpu }
                        "Processes"
                    }
                    Link { class: sub_cls(&Route::Events {}), to: Route::Events {},
                        Icon { width: 13, height: 13, icon: LdActivity }
                        "Events"
                    }
                    Link { class: sub_cls(&Route::Images {}), to: Route::Images {},
                        Icon { width: 13, height: 13, icon: LdPackage }
                        "Images"
                    }
                }

                // Footer
                div { class: "px-4 py-3 border-t border-slate-800/60",
                    p { class: "text-[10px] text-slate-700 font-mono", "v0.1.0+k3rs" }
                }
            }

            // ── Main ─────────────────────────────────────────────
            main { class: "ml-56 flex-1 min-h-screen",
                div { class: "max-w-screen-xl mx-auto px-8 py-8",
                    Outlet::<Route> {}
                }
            }
        }
    }
}

// ============================================================
// Shared types
// ============================================================
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ResourceRequirements {
    #[serde(default)]
    pub cpu_millis: u64,
    #[serde(default)]
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    pub registered_at: String,
    #[serde(default)]
    pub unschedulable: bool,
    #[serde(default)]
    pub capacity: ResourceRequirements,
    #[serde(default)]
    pub allocated: ResourceRequirements,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Pod {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub status: String,
    #[serde(default)]
    pub node_name: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ResourceQuota {
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub max_pods: Option<u32>,
    #[serde(default)]
    pub max_cpu_millis: Option<u64>,
    #[serde(default)]
    pub max_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NetworkPolicyObj {
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub pod_selector: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub policy_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PVC {
    pub id: String,
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub storage_class: Option<String>,
    #[serde(default)]
    pub requested_bytes: u64,
    #[serde(default)]
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ProcessInfo {
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cpu_percent: f32,
    #[serde(default)]
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ImageInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub size_human: String,
    #[serde(default)]
    pub layers: usize,
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub created: String,
}
