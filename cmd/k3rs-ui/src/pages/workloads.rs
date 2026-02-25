use crate::{api, ConfigMap, Deployment, Pod, Secret};
use dioxus::prelude::*;

/// Workloads page — tabbed view for Pods, Deployments, ConfigMaps, Secrets.
#[component]
pub fn Workloads() -> Element {
    let ns = use_context::<Signal<String>>();
    let mut active_tab = use_signal(|| "pods");

    let pods = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_pods(ns).await.unwrap_or_default() }
    });

    let deployments = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_deployments(ns).await.unwrap_or_default() }
    });

    let configmaps = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_configmaps(ns).await.unwrap_or_default() }
    });

    let secrets = use_resource(move || {
        let ns = ns.read().clone();
        async move { api::get_secrets(ns).await.unwrap_or_default() }
    });

    rsx! {
        div { class: "page-header",
            h2 { "Workloads" }
            p { "Manage pods, deployments, configmaps, and secrets" }
        }

        div { class: "tabs",
            button {
                class: if *active_tab.read() == "pods" { "tab active" } else { "tab" },
                onclick: move |_| active_tab.set("pods"),
                "Pods"
            }
            button {
                class: if *active_tab.read() == "deployments" { "tab active" } else { "tab" },
                onclick: move |_| active_tab.set("deployments"),
                "Deployments"
            }
            button {
                class: if *active_tab.read() == "configmaps" { "tab active" } else { "tab" },
                onclick: move |_| active_tab.set("configmaps"),
                "ConfigMaps"
            }
            button {
                class: if *active_tab.read() == "secrets" { "tab active" } else { "tab" },
                onclick: move |_| active_tab.set("secrets"),
                "Secrets"
            }
        }

        match *active_tab.read() {
            "pods" => rsx! { PodsTable { pods: pods.read().clone().unwrap_or_default() } },
            "deployments" => rsx! { DeploymentsTable { deployments: deployments.read().clone().unwrap_or_default() } },
            "configmaps" => rsx! { ConfigMapsTable { configmaps: configmaps.read().clone().unwrap_or_default() } },
            "secrets" => rsx! { SecretsTable { secrets: secrets.read().clone().unwrap_or_default() } },
            _ => rsx! { div { "Unknown tab" } },
        }
    }
}

#[component]
fn PodsTable(pods: Vec<Pod>) -> Element {
    rsx! {
        div { class: "data-table-wrap",
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Status" }
                        th { "Node" }
                        th { "ID" }
                    }
                }
                tbody {
                    if pods.is_empty() {
                        tr {
                            td { colspan: "4",
                                div { class: "empty-state",
                                    div { class: "icon", "📦" }
                                    p { "No pods found" }
                                }
                            }
                        }
                    } else {
                        for pod in pods.iter() {
                            tr {
                                td { "{pod.name}" }
                                td {
                                    span {
                                        class: format!("badge badge-{}", pod.status.to_lowercase()),
                                        "{pod.status}"
                                    }
                                }
                                td { "{pod.node_id.as_deref().unwrap_or(\"—\")}" }
                                td { class: "mono", "{pod.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn DeploymentsTable(deployments: Vec<Deployment>) -> Element {
    rsx! {
        div { class: "data-table-wrap",
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Replicas" }
                        th { "ID" }
                    }
                }
                tbody {
                    if deployments.is_empty() {
                        tr {
                            td { colspan: "3",
                                div { class: "empty-state",
                                    div { class: "icon", "🚀" }
                                    p { "No deployments found" }
                                }
                            }
                        }
                    } else {
                        for dep in deployments.iter() {
                            tr {
                                td { "{dep.name}" }
                                td { "{dep.spec.replicas}" }
                                td { class: "mono", "{dep.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ConfigMapsTable(configmaps: Vec<ConfigMap>) -> Element {
    rsx! {
        div { class: "data-table-wrap",
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Keys" }
                        th { "ID" }
                    }
                }
                tbody {
                    if configmaps.is_empty() {
                        tr {
                            td { colspan: "3",
                                div { class: "empty-state",
                                    div { class: "icon", "📝" }
                                    p { "No configmaps found" }
                                }
                            }
                        }
                    } else {
                        for cm in configmaps.iter() {
                            tr {
                                td { "{cm.name}" }
                                td { "{cm.data.len()}" }
                                td { class: "mono", "{cm.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SecretsTable(secrets: Vec<Secret>) -> Element {
    rsx! {
        div { class: "data-table-wrap",
            table { class: "data-table",
                thead {
                    tr {
                        th { "Name" }
                        th { "Keys" }
                        th { "ID" }
                    }
                }
                tbody {
                    if secrets.is_empty() {
                        tr {
                            td { colspan: "3",
                                div { class: "empty-state",
                                    div { class: "icon", "🔒" }
                                    p { "No secrets found" }
                                }
                            }
                        }
                    } else {
                        for s in secrets.iter() {
                            tr {
                                td { "{s.name}" }
                                td { "{s.data.len()}" }
                                td { class: "mono", "{s.id}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
