use crate::api;
use dioxus::prelude::*;

#[component]
pub fn Services() -> Element {
    let ns = use_context::<Signal<String>>();
    let services = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_services(ns).await.unwrap_or_default() }
    });
    let svcs_data = services.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Services" }
            p { class: "text-sm text-slate-400 mt-1", "Service discovery and load balancing" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Type" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Cluster IP" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Ports" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "ID" }
                    }
                }
                tbody {
                    if let Some(svcs) = svcs_data.as_ref() {
                        if svcs.is_empty() {
                            tr { td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No services found" } }
                        } else {
                            for svc in svcs.iter() {
                                {
                                    let svc_type = &svc.spec.service_type;
                                    let type_cls = match svc_type.as_str() {
                                        "NodePort" => "bg-violet-500/10 text-violet-400 border border-violet-500/20",
                                        "LoadBalancer" => "bg-cyan-500/10 text-cyan-400 border border-cyan-500/20",
                                        _ => "bg-blue-500/10 text-blue-400 border border-blue-500/20",
                                    };
                                    let ports_str = svc.spec.ports.iter()
                                        .map(|p| format!("{}:{}", p.port, p.target_port))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let ports_display = if ports_str.is_empty() { "—".to_string() } else { ports_str };
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{svc.name}" }
                                            td { class: "px-5 py-3",
                                                span { class: "inline-block px-2.5 py-0.5 rounded-full text-[11px] font-medium {type_cls}", "{svc_type}" }
                                            }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{svc.cluster_ip.as_deref().unwrap_or(\"—\")}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{ports_display}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-600", "{svc.id}" }
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
