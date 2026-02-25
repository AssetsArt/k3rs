use crate::api;
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

#[component]
pub fn Nodes() -> Element {
    let nodes = use_resource(move || async move { api::get_nodes().await.unwrap_or_default() });
    let nodes_data = nodes.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Nodes" }
            p { class: "text-sm text-slate-400 mt-1", "Cluster node management" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Status" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Labels" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Registered" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if let Some(nodes) = nodes_data.as_ref() {
                        if nodes.is_empty() {
                            tr {
                                td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No nodes registered yet" }
                            }
                        } else {
                            for node in nodes.iter() {
                                {
                                    let labels_str = node.labels.iter()
                                        .map(|(k, v)| format!("{}={}", k, v))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let labels_display = if labels_str.is_empty() { "—".to_string() } else { labels_str };
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{node.name}" }
                                            td { class: "px-5 py-3", StatusBadge { status: node.status.clone() } }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{labels_display}" }
                                            td { class: "px-5 py-3 text-xs text-slate-500", "{node.registered_at}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{node.id}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
