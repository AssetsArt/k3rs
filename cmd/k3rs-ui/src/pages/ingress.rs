use crate::api;
use dioxus::prelude::*;

/// A flattened row for displaying ingress rules in a table.
struct IngressRow {
    name: String,
    id: String,
    host: String,
    path: String,
    backend: String,
}

/// Ingress & Networking page — view Ingress rules.
#[component]
pub fn Ingress() -> Element {
    let ns = use_context::<Signal<String>>();

    let ingresses = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_ingresses(ns).await.unwrap_or_default() }
    });

    let ing_data = ingresses.read();

    // Flatten ingress rules into rows
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
        div { class: "page-header",
            h2 { "Ingress & Networking" }
            p { "External traffic routing rules" }
        }

        div { class: "data-table-wrap",
            div { class: "table-header",
                h3 { "Ingress Rules" }
            }
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Host" }
                        th { "Path" }
                        th { "Backend" }
                        th { "ID" }
                    }
                }
                tbody {
                    if rows.is_empty() {
                        tr {
                            td { colspan: "5",
                                div { class: "empty-state",
                                    div { class: "icon", "🌐" }
                                    p { "No ingress rules found" }
                                }
                            }
                        }
                    } else {
                        for row in rows.iter() {
                            tr {
                                td { "{row.name}" }
                                td { "{row.host}" }
                                td { class: "mono", "{row.path}" }
                                td { class: "mono", "{row.backend}" }
                                td { class: "mono", "{row.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
