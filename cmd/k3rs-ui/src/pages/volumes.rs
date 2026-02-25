use crate::api;
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

#[component]
pub fn Volumes() -> Element {
    let ns = use_context::<Signal<String>>();
    let pvcs = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_pvcs(ns).await.unwrap_or_default() }
    });
    let pvcs_data = pvcs.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Persistent Volume Claims" }
            p { class: "text-sm text-slate-400 mt-1", "Storage claims in this namespace" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Namespace" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Storage Class" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Requested" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Phase" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if let Some(items) = pvcs_data.as_ref() {
                        if items.is_empty() {
                            tr {
                                td { colspan: "6", class: "text-center py-16 text-slate-500 text-sm", "No persistent volume claims" }
                            }
                        } else {
                            for pvc in items.iter() {
                                {
                                    let storage = if pvc.requested_bytes >= 1_000_000_000 {
                                        format!("{:.1} GB", pvc.requested_bytes as f64 / 1_000_000_000.0)
                                    } else if pvc.requested_bytes >= 1_000_000 {
                                        format!("{} MB", pvc.requested_bytes / 1_000_000)
                                    } else {
                                        format!("{} bytes", pvc.requested_bytes)
                                    };
                                    let sc = pvc.storage_class.as_deref().unwrap_or("default");
                                    let phase_status = if pvc.phase.is_empty() { "Pending".to_string() } else { pvc.phase.clone() };
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{pvc.name}" }
                                            td { class: "px-5 py-3 text-sm text-slate-400", "{pvc.namespace}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{sc}" }
                                            td { class: "px-5 py-3 text-sm text-violet-400 font-mono", "{storage}" }
                                            td { class: "px-5 py-3", StatusBadge { status: phase_status } }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{pvc.id}" }
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
