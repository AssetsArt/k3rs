use crate::api;
use dioxus::prelude::*;

/// Services page — list/create/delete Services.
#[component]
pub fn Services() -> Element {
    let ns = use_context::<Signal<String>>();

    let services = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_services(ns).await.unwrap_or_default() }
    });

    let svcs_data = services.read();

    rsx! {
        div { class: "page-header",
            h2 { "Services" }
            p { "Service discovery and load balancing" }
        }

        div { class: "data-table-wrap",
            div { class: "table-header",
                h3 { "All Services" }
            }
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Type" }
                        th { "Cluster IP" }
                        th { "Ports" }
                        th { "ID" }
                    }
                }
                tbody {
                    if let Some(svcs) = svcs_data.as_ref() {
                        if svcs.is_empty() {
                            tr {
                                td { colspan: "5",
                                    div { class: "empty-state",
                                        div { class: "icon", "🔗" }
                                        p { "No services found" }
                                    }
                                }
                            }
                        } else {
                            for svc in svcs.iter() {
                                {
                                    let svc_type = &svc.spec.service_type;
                                    let badge_class = match svc_type.as_str() {
                                        "NodePort" => "badge badge-nodeport",
                                        "LoadBalancer" => "badge badge-loadbalancer",
                                        _ => "badge badge-clusterip",
                                    };
                                    let ports_str = svc.spec.ports.iter()
                                        .map(|p| format!("{}→{}", p.port, p.target_port))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let ports_display = if ports_str.is_empty() { "—".to_string() } else { ports_str };
                                    rsx! {
                                        tr {
                                            td { "{svc.name}" }
                                            td {
                                                span { class: badge_class, "{svc_type}" }
                                            }
                                            td { class: "mono",
                                                "{svc.cluster_ip.as_deref().unwrap_or(\"—\")}"
                                            }
                                            td { class: "mono", "{ports_display}" }
                                            td { class: "mono", "{svc.id}" }
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
