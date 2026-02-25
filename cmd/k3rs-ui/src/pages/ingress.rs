use crate::api;
use dioxus::prelude::*;

struct IngressRow {
    name: String,
    id: String,
    host: String,
    path: String,
    backend: String,
}

#[component]
pub fn Ingress() -> Element {
    let ns = use_context::<Signal<String>>();
    let ingresses = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_ingresses(ns).await.unwrap_or_default() }
    });
    let ing_data = ingresses.read();

    let rows: Vec<IngressRow> = ing_data
        .as_ref()
        .map(|ings| {
            ings.iter()
                .flat_map(|ing| {
                    ing.spec.rules.iter().flat_map(move |rule| {
                        rule.http.paths.iter().map(move |path| IngressRow {
                            name: ing.name.clone(),
                            id: ing.id.clone(),
                            host: rule.host.clone(),
                            path: path.path.clone(),
                            backend: format!(
                                "{}:{}",
                                path.backend.service_name, path.backend.service_port
                            ),
                        })
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Ingress" }
            p { class: "text-sm text-slate-400 mt-1", "External traffic routing" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Host" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Path" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Backend" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if rows.is_empty() {
                        tr { td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No ingress rules found" } }
                    } else {
                        for row in rows.iter() {
                            tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{row.name}" }
                                td { class: "px-5 py-3 text-sm text-slate-400", "{row.host}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{row.path}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{row.backend}" }
                                td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{row.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
