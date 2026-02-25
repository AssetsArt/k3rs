use crate::api;
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

#[component]
pub fn Pods() -> Element {
    let ns = use_context::<Signal<String>>();
    let pods = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_pods(ns).await.unwrap_or_default() }
    });
    let data = pods.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Pods" }
            p { class: "text-sm text-slate-400 mt-1", "Running container instances" }
        }

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
                    if let Some(pods) = data.as_ref() {
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
}
