use crate::api;
use dioxus::prelude::*;

#[component]
pub fn Quotas() -> Element {
    let ns = use_context::<Signal<String>>();
    let quotas = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_quotas(ns).await.unwrap_or_default() }
    });
    let quotas_data = quotas.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Resource Quotas" }
            p { class: "text-sm text-slate-400 mt-1", "Namespace resource limits" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Namespace" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Max Pods" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Max CPU (cores)" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Max Memory" }
                    }
                }
                tbody {
                    if let Some(items) = quotas_data.as_ref() {
                        if items.is_empty() {
                            tr {
                                td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No resource quotas defined" }
                            }
                        } else {
                            for q in items.iter() {
                                {
                                    let cpu = q.max_cpu_millis.map(|c| format!("{:.1}", c as f64 / 1000.0)).unwrap_or("—".into());
                                    let mem = q.max_memory_bytes.map(|m| format!("{} MB", m / 1_000_000)).unwrap_or("—".into());
                                    let pods = q.max_pods.map(|p| p.to_string()).unwrap_or("—".into());
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{q.name}" }
                                            td { class: "px-5 py-3 text-sm text-slate-400", "{q.namespace}" }
                                            td { class: "px-5 py-3 text-sm text-cyan-400 font-mono", "{pods}" }
                                            td { class: "px-5 py-3 text-sm text-cyan-400 font-mono", "{cpu}" }
                                            td { class: "px-5 py-3 text-sm text-cyan-400 font-mono", "{mem}" }
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
