use crate::api;
use dioxus::prelude::*;

#[component]
pub fn ConfigMaps() -> Element {
    let ns = use_context::<Signal<String>>();
    let configmaps = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_configmaps(ns).await.unwrap_or_default() }
    });
    let data = configmaps.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "ConfigMaps" }
            p { class: "text-sm text-slate-400 mt-1", "Configuration data storage" }
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
                    if let Some(cms) = data.as_ref() {
                        if cms.is_empty() {
                            tr { td { colspan: "4", class: "text-center py-16 text-slate-500 text-sm", "No configmaps found" } }
                        } else {
                            for cm in cms.iter() {
                                tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                    td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{cm.name}" }
                                    td { class: "px-5 py-3 text-sm text-slate-400", "{cm.data.len()}" }
                                    td { class: "px-5 py-3 text-xs text-slate-500", "{cm.namespace}" }
                                    td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{cm.id}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
