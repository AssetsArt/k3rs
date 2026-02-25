use crate::api;
use dioxus::prelude::*;

/// Nodes page — list all cluster nodes with status and metadata.
#[component]
pub fn Nodes() -> Element {
    let nodes = use_resource(move || async move { api::get_nodes().await.unwrap_or_default() });

    let nodes_data = nodes.read();

    rsx! {
        div { class: "page-header",
            h2 { "Nodes" }
            p { "Cluster node management" }
        }

        div { class: "data-table-wrap",
            div { class: "table-header",
                h3 { "All Nodes" }
            }
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Status" }
                        th { "Labels" }
                        th { "Registered" }
                        th { "ID" }
                    }
                }
                tbody {
                    if let Some(nodes) = nodes_data.as_ref() {
                        if nodes.is_empty() {
                            tr {
                                td { colspan: "5",
                                    div { class: "empty-state",
                                        div { class: "icon", "🖥️" }
                                        p { "No nodes registered yet" }
                                    }
                                }
                            }
                        } else {
                            for node in nodes.iter() {
                                {
                                    let labels_str = node.labels.iter()
                                        .map(|(k, v)| format!("{}={}", k, v))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let labels_display = if labels_str.is_empty() { "—".to_string() } else { labels_str };
                                    rsx! {
                                        tr {
                                            td { "{node.name}" }
                                            td {
                                                span {
                                                    class: format!("badge badge-{}", node.status.to_lowercase()),
                                                    "{node.status}"
                                                }
                                            }
                                            td { class: "mono", "{labels_display}" }
                                            td { "{node.registered_at}" }
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
    }
}
