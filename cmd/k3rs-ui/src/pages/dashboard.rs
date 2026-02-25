use crate::api;
use dioxus::prelude::*;
use dioxus_free_icons::icons::ld_icons::*;
use dioxus_free_icons::Icon;

#[component]
pub fn Dashboard() -> Element {
    let ns = use_context::<Signal<String>>();

    let cluster_info = use_resource(move || async move { api::get_cluster_info().await.ok() });
    let nodes = use_resource(move || async move { api::get_nodes().await.unwrap_or_default() });
    let pods = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_pods(ns).await.unwrap_or_default() }
    });
    let services = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_services(ns).await.unwrap_or_default() }
    });

    let info = cluster_info.read();
    let nodes_data = nodes.read();
    let pods_data = pods.read();
    let svcs_data = services.read();

    let node_count = nodes_data.as_ref().map(|n| n.len()).unwrap_or(0);
    let ready_nodes = nodes_data
        .as_ref()
        .map(|n| n.iter().filter(|n| n.status == "Ready").count())
        .unwrap_or(0);
    let pod_count = pods_data.as_ref().map(|p| p.len()).unwrap_or(0);
    let running_pods = pods_data
        .as_ref()
        .map(|p| p.iter().filter(|p| p.status == "Running").count())
        .unwrap_or(0);
    let svc_count = svcs_data.as_ref().map(|s| s.len()).unwrap_or(0);

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Dashboard" }
            p { class: "text-sm text-slate-400 mt-1", "Cluster overview" }
        }

        // Stat cards — inlined icons directly
        div { class: "grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 mb-8",
            // Nodes
            div { class: "bg-slate-900 border border-slate-800 rounded-xl p-5 hover:border-emerald-500/40 transition-all",
                div { class: "flex items-center justify-between mb-3",
                    span { class: "text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Nodes" }
                    span { class: "text-emerald-400", Icon { width: 18, height: 18, icon: LdServer } }
                }
                div { class: "text-2xl font-bold text-emerald-400", "{node_count}" }
                div { class: "text-xs text-slate-500 mt-1", "{ready_nodes} ready" }
            }
            // Pods
            div { class: "bg-slate-900 border border-slate-800 rounded-xl p-5 hover:border-blue-500/40 transition-all",
                div { class: "flex items-center justify-between mb-3",
                    span { class: "text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Pods" }
                    span { class: "text-blue-400", Icon { width: 18, height: 18, icon: LdBox } }
                }
                div { class: "text-2xl font-bold text-blue-400", "{pod_count}" }
                div { class: "text-xs text-slate-500 mt-1", "{running_pods} running" }
            }
            // Services
            div { class: "bg-slate-900 border border-slate-800 rounded-xl p-5 hover:border-violet-500/40 transition-all",
                div { class: "flex items-center justify-between mb-3",
                    span { class: "text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Services" }
                    span { class: "text-violet-400", Icon { width: 18, height: 18, icon: LdNetwork } }
                }
                div { class: "text-2xl font-bold text-violet-400", "{svc_count}" }
                div { class: "text-xs text-slate-500 mt-1", "current namespace" }
            }
            // Version
            if let Some(Some(ci)) = info.as_ref() {
                div { class: "bg-slate-900 border border-slate-800 rounded-xl p-5 hover:border-amber-500/40 transition-all",
                    div { class: "flex items-center justify-between mb-3",
                        span { class: "text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Version" }
                        span { class: "text-amber-400", Icon { width: 18, height: 18, icon: LdInfo } }
                    }
                    div { class: "text-2xl font-bold text-amber-400", "{ci.version}" }
                    div { class: "text-xs text-slate-500 mt-1", "{ci.state_store}" }
                }
            }
        }

        // Nodes table
        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            div { class: "px-5 py-3.5 border-b border-slate-800",
                h3 { class: "text-sm font-semibold text-white", "Recent Nodes" }
            }
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Status" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if let Some(nodes) = nodes_data.as_ref() {
                        if nodes.is_empty() {
                            tr { td { colspan: "3", class: "text-center py-12 text-slate-500 text-sm", "No nodes registered" } }
                        } else {
                            for node in nodes.iter() {
                                tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                    td { class: "px-5 py-3 text-sm text-slate-300", "{node.name}" }
                                    td { class: "px-5 py-3", StatusBadge { status: node.status.clone() } }
                                    td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{node.id}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn StatusBadge(status: String) -> Element {
    let cls = match status.as_str() {
        "Ready" | "Running" => "bg-emerald-500/10 text-emerald-400 border border-emerald-500/20",
        "NotReady" | "Pending" | "Scheduled" => {
            "bg-amber-500/10 text-amber-400 border border-amber-500/20"
        }
        "Failed" | "Terminated" => "bg-red-500/10 text-red-400 border border-red-500/20",
        _ => "bg-slate-500/10 text-slate-400 border border-slate-500/20",
    };
    rsx! {
        span { class: "inline-block px-2.5 py-0.5 rounded-full text-[11px] font-medium {cls}", "{status}" }
    }
}
