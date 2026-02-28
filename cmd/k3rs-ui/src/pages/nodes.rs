use crate::api;
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

fn fmt_cpu(millis: u64) -> String {
    if millis >= 1000 {
        format!("{:.1}CPU", millis as f64 / 1000.0)
    } else {
        format!("{}m", millis)
    }
}

fn fmt_mem(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1}Gi", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.0}Mi", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{}Ki", bytes / 1024)
    } else {
        format!("{}B", bytes)
    }
}

#[component]
pub fn Nodes() -> Element {
    let nodes = use_resource(move || async move { api::get_nodes().await.unwrap_or_default() });
    let nodes_data = nodes.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Nodes" }
            p { class: "text-sm text-slate-400 mt-1", "Cluster node management" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "Name"
                        }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "Status"
                        }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "CPU"
                        }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "Memory"
                        }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "Labels"
                        }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "Registered"
                        }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold",
                            "ID"
                        }
                    }
                }
                tbody {
                    if let Some(nodes) = nodes_data.as_ref() {
                        if nodes.is_empty() {
                            tr {
                                td {
                                    colspan: "7",
                                    class: "text-center py-16 text-slate-500 text-sm",
                                    "No nodes registered yet"
                                }
                            }
                        } else {
                            for node in nodes.iter() {
                                {
                                    let labels_str = node
                                        .labels
                                        .iter()
                                        .map(|(k, v)| format!("{}={}", k, v))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let labels_display = if labels_str.is_empty() {
                                        "—".to_string()
                                    } else {
                                        labels_str
                                    };

                                    let cpu_cap = node.capacity.cpu_millis;
                                    let cpu_alloc = node.allocated.cpu_millis;
                                    let cpu_pct = if cpu_cap > 0 {
                                        (cpu_alloc * 100 / cpu_cap).min(100)
                                    } else {
                                        0
                                    };
                                    let cpu_bar_cls = if cpu_pct >= 80 {
                                        "h-1 rounded-full bg-red-500"
                                    } else if cpu_pct >= 60 {
                                        "h-1 rounded-full bg-yellow-500"
                                    } else {
                                        "h-1 rounded-full bg-emerald-500"
                                    };

                                    let mem_cap = node.capacity.memory_bytes;
                                    let mem_alloc = node.allocated.memory_bytes;
                                    let mem_pct = if mem_cap > 0 {
                                        (mem_alloc * 100 / mem_cap).min(100)
                                    } else {
                                        0
                                    };
                                    let mem_bar_cls = if mem_pct >= 80 {
                                        "h-1 rounded-full bg-red-500"
                                    } else if mem_pct >= 60 {
                                        "h-1 rounded-full bg-yellow-500"
                                    } else {
                                        "h-1 rounded-full bg-emerald-500"
                                    };

                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium",
                                                "{node.name}"
                                            }
                                            td { class: "px-5 py-3",
                                                StatusBadge { status: node.status.clone() }
                                            }
                                            // CPU column
                                            td { class: "px-5 py-3",
                                                if cpu_cap == 0 {
                                                    span { class: "text-xs text-slate-600", "—" }
                                                } else {
                                                    div { class: "flex flex-col gap-1 min-w-[100px]",
                                                        div { class: "flex justify-between text-[11px]",
                                                            span { class: "text-slate-300",
                                                                "{fmt_cpu(cpu_alloc)}"
                                                            }
                                                            span { class: "text-slate-600",
                                                                "/ {fmt_cpu(cpu_cap)}"
                                                            }
                                                        }
                                                        div { class: "w-full bg-slate-800 rounded-full h-1",
                                                            div {
                                                                class: "{cpu_bar_cls}",
                                                                style: "width: {cpu_pct}%",
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            // Memory column
                                            td { class: "px-5 py-3",
                                                if mem_cap == 0 {
                                                    span { class: "text-xs text-slate-600", "—" }
                                                } else {
                                                    div { class: "flex flex-col gap-1 min-w-[110px]",
                                                        div { class: "flex justify-between text-[11px]",
                                                            span { class: "text-slate-300",
                                                                "{fmt_mem(mem_alloc)}"
                                                            }
                                                            span { class: "text-slate-600",
                                                                "/ {fmt_mem(mem_cap)}"
                                                            }
                                                        }
                                                        div { class: "w-full bg-slate-800 rounded-full h-1",
                                                            div {
                                                                class: "{mem_bar_cls}",
                                                                style: "width: {mem_pct}%",
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-500",
                                                "{labels_display}"
                                            }
                                            td { class: "px-5 py-3 text-xs text-slate-500",
                                                "{node.registered_at}"
                                            }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-600",
                                                "{node.id}"
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
