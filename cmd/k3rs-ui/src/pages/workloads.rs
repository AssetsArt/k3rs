use crate::{api, ConfigMap, Deployment, Pod, Secret};
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

#[component]
pub fn Workloads() -> Element {
    let ns = use_context::<Signal<String>>();
    let mut active_tab = use_signal(|| "pods");

    let pods = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_pods(ns).await.unwrap_or_default() }
    });
    let deployments = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_deployments(ns).await.unwrap_or_default() }
    });
    let configmaps = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_configmaps(ns).await.unwrap_or_default() }
    });
    let secrets = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_secrets(ns).await.unwrap_or_default() }
    });

    let tabs = vec!["pods", "deployments", "configmaps", "secrets"];

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Workloads" }
            p { class: "text-sm text-slate-400 mt-1", "Manage pods, deployments, configmaps, and secrets" }
        }

        // Tabs
        div { class: "flex gap-1 border-b border-slate-800 mb-5",
            for tab in tabs {
                button {
                    class: if *active_tab.read() == tab {
                        "px-4 py-2.5 text-sm font-medium text-blue-400 border-b-2 border-blue-400 -mb-px transition-colors"
                    } else {
                        "px-4 py-2.5 text-sm font-medium text-slate-500 border-b-2 border-transparent hover:text-slate-300 -mb-px transition-colors"
                    },
                    onclick: move |_| active_tab.set(tab),
                    "{tab}"
                }
            }
        }

        match *active_tab.read() {
            "pods" => rsx! { PodsTable { pods: pods.read().clone().unwrap_or_default() } },
            "deployments" => rsx! { DeploymentsTable { deployments: deployments.read().clone().unwrap_or_default() } },
            "configmaps" => rsx! { ConfigMapsTable { configmaps: configmaps.read().clone().unwrap_or_default() } },
            "secrets" => rsx! { SecretsTable { secrets: secrets.read().clone().unwrap_or_default() } },
            _ => rsx! { div {} },
        }
    }
}

#[component]
fn PodsTable(pods: Vec<Pod>) -> Element {
    rsx! {
        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Status" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Node" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if pods.is_empty() {
                        tr { td { colspan: "4", class: "text-center py-16 text-slate-500 text-sm", "No pods found" } }
                    } else {
                        for pod in pods.iter() {
                            tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{pod.name}" }
                                td { class: "px-5 py-3", StatusBadge { status: pod.status.clone() } }
                                td { class: "px-5 py-3 text-xs text-slate-500", "{pod.node_id.as_deref().unwrap_or(\"—\")}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{pod.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn DeploymentsTable(deployments: Vec<Deployment>) -> Element {
    rsx! {
        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Replicas" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if deployments.is_empty() {
                        tr { td { colspan: "3", class: "text-center py-16 text-slate-500 text-sm", "No deployments found" } }
                    } else {
                        for dep in deployments.iter() {
                            tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{dep.name}" }
                                td { class: "px-5 py-3 text-sm text-slate-400", "{dep.spec.replicas}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{dep.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ConfigMapsTable(configmaps: Vec<ConfigMap>) -> Element {
    rsx! {
        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Keys" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if configmaps.is_empty() {
                        tr { td { colspan: "3", class: "text-center py-16 text-slate-500 text-sm", "No configmaps found" } }
                    } else {
                        for cm in configmaps.iter() {
                            tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{cm.name}" }
                                td { class: "px-5 py-3 text-sm text-slate-400", "{cm.data.len()}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{cm.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SecretsTable(secrets: Vec<Secret>) -> Element {
    rsx! {
        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Keys" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if secrets.is_empty() {
                        tr { td { colspan: "3", class: "text-center py-16 text-slate-500 text-sm", "No secrets found" } }
                    } else {
                        for s in secrets.iter() {
                            tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{s.name}" }
                                td { class: "px-5 py-3 text-sm text-slate-400", "{s.data.len()}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{s.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
