use crate::api;
use dioxus::prelude::*;

#[component]
pub fn NetworkPolicies() -> Element {
    let ns = use_context::<Signal<String>>();
    let policies = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_network_policies(ns).await.unwrap_or_default() }
    });
    let policies_data = policies.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Network Policies" }
            p { class: "text-sm text-slate-400 mt-1", "Traffic rules for pods in this namespace" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Namespace" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Pod Selector" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Policy Types" }
                    }
                }
                tbody {
                    if let Some(items) = policies_data.as_ref() {
                        if items.is_empty() {
                            tr {
                                td { colspan: "4", class: "text-center py-16 text-slate-500 text-sm", "No network policies defined" }
                            }
                        } else {
                            for pol in items.iter() {
                                {
                                    let selector_str = pol.pod_selector.iter()
                                        .map(|(k, v)| format!("{}={}", k, v))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let selector_display = if selector_str.is_empty() { "all pods".to_string() } else { selector_str };
                                    let types_str = if pol.policy_types.is_empty() {
                                        "—".to_string()
                                    } else {
                                        pol.policy_types.join(", ")
                                    };
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{pol.name}" }
                                            td { class: "px-5 py-3 text-sm text-slate-400", "{pol.namespace}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-500", "{selector_display}" }
                                            td { class: "px-5 py-3",
                                                for t in pol.policy_types.iter() {
                                                    {
                                                        let badge_cls = match t.as_str() {
                                                            "Ingress" => "bg-blue-500/10 text-blue-400 border border-blue-500/20",
                                                            "Egress" => "bg-amber-500/10 text-amber-400 border border-amber-500/20",
                                                            _ => "bg-slate-500/10 text-slate-400 border border-slate-500/20",
                                                        };
                                                        rsx! {
                                                            span { class: "inline-block px-2 py-0.5 rounded-full text-[11px] font-medium mr-1 {badge_cls}", "{t}" }
                                                        }
                                                    }
                                                }
                                                if pol.policy_types.is_empty() {
                                                    span { class: "text-xs text-slate-500", "{types_str}" }
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
    }
}
