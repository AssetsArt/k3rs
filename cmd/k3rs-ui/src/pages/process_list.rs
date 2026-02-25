use crate::api;
use dioxus::prelude::*;

#[component]
pub fn ProcessList() -> Element {
    let processes =
        use_resource(move || async move { api::get_processes().await.unwrap_or_default() });
    let procs_data = processes.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Process List" }
            p { class: "text-sm text-slate-400 mt-1", "Running processes across cluster nodes" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Node" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Process Name" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "CPU %" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Memory" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "PID" }
                    }
                }
                tbody {
                    if let Some(procs) = procs_data.as_ref() {
                        if procs.is_empty() {
                            tr {
                                td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No processes found" }
                            }
                        } else {
                            for p in procs.iter() {
                                {
                                    let mem = if p.memory_bytes >= 1_000_000_000 {
                                        format!("{:.1} GB", p.memory_bytes as f64 / 1_073_741_824.0)
                                    } else if p.memory_bytes >= 1_000_000 {
                                        format!("{:.1} MB", p.memory_bytes as f64 / 1_048_576.0)
                                    } else if p.memory_bytes >= 1_000 {
                                        format!("{:.0} KB", p.memory_bytes as f64 / 1_024.0)
                                    } else {
                                        format!("{} B", p.memory_bytes)
                                    };
                                    let cpu_str = format!("{:.1}", p.cpu_percent);
                                    let cpu_cls = if p.cpu_percent > 50.0 {
                                        "text-red-400"
                                    } else if p.cpu_percent > 10.0 {
                                        "text-amber-400"
                                    } else {
                                        "text-emerald-400"
                                    };
                                    let mem_cls = if p.memory_bytes > 500_000_000 {
                                        "text-red-400"
                                    } else if p.memory_bytes > 100_000_000 {
                                        "text-amber-400"
                                    } else {
                                        "text-cyan-400"
                                    };
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-2.5 text-sm text-slate-400", "{p.node_name}" }
                                            td { class: "px-5 py-2.5 text-sm text-slate-300 font-medium", "{p.name}" }
                                            td { class: "px-5 py-2.5 text-sm font-mono text-right {cpu_cls}", "{cpu_str}" }
                                            td { class: "px-5 py-2.5 text-sm font-mono text-right {mem_cls}", "{mem}" }
                                            td { class: "px-5 py-2.5 text-xs font-mono text-right text-slate-500", "{p.pid}" }
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
