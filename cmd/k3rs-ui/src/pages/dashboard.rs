use crate::api;
use dioxus::prelude::*;

/// Dashboard page — cluster overview with status cards and recent resources.
#[component]
pub fn Dashboard() -> Element {
    let ns = use_context::<Signal<String>>();

    let cluster_info = use_resource(move || async move { api::get_cluster_info().await.ok() });

    let nodes = use_resource(move || async move { api::get_nodes().await.unwrap_or_default() });

    let pods = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_pods(ns).await.unwrap_or_default() }
    });

    let services = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_services(ns).await.unwrap_or_default() }
    });

    let info = cluster_info.read();
    let nodes_data = nodes.read();
    let pods_data = pods.read();
    let svcs_data = services.read();

    let node_count = nodes_data.as_ref().map(|n| n.len()).unwrap_or(0);
    let ready_nodes = nodes_data
        .as_ref()
        .map(|n| n.iter().filter(|n| n.status == "Ready").count())
        .unwrap_or(0);
    let pod_count = pods_data.as_ref().map(|p| p.len()).unwrap_or(0);
    let running_pods = pods_data
        .as_ref()
        .map(|p| p.iter().filter(|p| p.status == "Running").count())
        .unwrap_or(0);
    let svc_count = svcs_data.as_ref().map(|s| s.len()).unwrap_or(0);

    rsx! {
        div { class: "page-header",
            h2 { "Dashboard" }
            p { "Cluster overview and resource summary" }
        }

        div { class: "stats-grid",
            div { class: "stat-card success",
                div { class: "label", "Nodes" }
                div { class: "value", "{node_count}" }
                div { class: "sub", "{ready_nodes} ready" }
            }
            div { class: "stat-card info",
                div { class: "label", "Pods" }
                div { class: "value", "{pod_count}" }
                div { class: "sub", "{running_pods} running" }
            }
            div { class: "stat-card accent",
                div { class: "label", "Services" }
                div { class: "value", "{svc_count}" }
                div { class: "sub", "in current namespace" }
            }
            if let Some(Some(ci)) = info.as_ref() {
                div { class: "stat-card warning",
                    div { class: "label", "Version" }
                    div { class: "value", style: "font-size: 20px;", "{ci.version}" }
                    div { class: "sub", "{ci.state_store}" }
                }
            }
        }

        div { class: "data-table-wrap",
            div { class: "table-header",
                h3 { "Nodes" }
            }
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Status" }
                        th { "ID" }
                    }
                }
                tbody {
                    if let Some(nodes) = nodes_data.as_ref() {
                        if nodes.is_empty() {
                            tr {
                                td { colspan: "3",
                                    div { class: "empty-state",
                                        div { class: "icon", "🖥️" }
                                        p { "No nodes registered" }
                                    }
                                }
                            }
                        } else {
                            for node in nodes.iter() {
                                tr {
                                    td { "{node.name}" }
                                    td {
                                        span {
                                            class: format!("badge badge-{}", node.status.to_lowercase()),
                                            "{node.status}"
                                        }
                                    }
                                    td { class: "mono", "{node.id}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
