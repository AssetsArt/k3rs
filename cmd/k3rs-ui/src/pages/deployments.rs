use crate::api;
use dioxus::prelude::*;

#[component]
pub fn Deployments() -> Element {
    let ns = use_context::<Signal<String>>();
    let deployments = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_deployments(ns).await.unwrap_or_default() }
    });
    let data = deployments.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Deployments" }
            p { class: "text-sm text-slate-400 mt-1", "Application deployment management" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Replicas" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Namespace" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if let Some(deps) = data.as_ref() {
                        if deps.is_empty() {
                            tr { td { colspan: "4", class: "text-center py-16 text-slate-500 text-sm", "No deployments found" } }
                        } else {
                            for dep in deps.iter() {
                                tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                    td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{dep.name}" }
                                    td { class: "px-5 py-3 text-sm text-slate-400", "{dep.spec.replicas}" }
                                    td { class: "px-5 py-3 text-xs text-slate-500", "{dep.namespace}" }
                                    td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{dep.id}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
