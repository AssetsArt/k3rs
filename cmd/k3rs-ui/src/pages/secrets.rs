use crate::api;
use dioxus::prelude::*;

#[component]
pub fn Secrets() -> Element {
    let ns = use_context::<Signal<String>>();
    let secrets = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_secrets(ns).await.unwrap_or_default() }
    });
    let data = secrets.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Secrets" }
            p { class: "text-sm text-slate-400 mt-1", "Sensitive configuration data" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Keys" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Namespace" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if let Some(secrets) = data.as_ref() {
                        if secrets.is_empty() {
                            tr { td { colspan: "4", class: "text-center py-16 text-slate-500 text-sm", "No secrets found" } }
                        } else {
                            for s in secrets.iter() {
                                tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                    td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{s.name}" }
                                    td { class: "px-5 py-3 text-sm text-slate-400", "{s.data.len()}" }
                                    td { class: "px-5 py-3 text-xs text-slate-500", "{s.namespace}" }
                                    td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{s.id}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
