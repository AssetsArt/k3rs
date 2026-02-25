use crate::api;
use dioxus::prelude::*;
use dioxus_free_icons::icons::ld_icons::*;
use dioxus_free_icons::Icon;

#[component]
pub fn ProcessList() -> Element {
    let mut refresh_tick = use_signal(|| 0u32);
    let mut auto_reload = use_signal(|| true);
    let mut interval_secs = use_signal(|| 3u64);

    // Auto-reload timer
    use_future(move || async move {
        loop {
            let secs = *interval_secs.read();
            let dur = std::time::Duration::from_secs(secs);
            #[cfg(feature = "server")]
            tokio::time::sleep(dur).await;
            #[cfg(all(feature = "web", not(feature = "server")))]
            gloo_timers::future::sleep(dur).await;
            let is_auto = *auto_reload.read();
            if is_auto {
                refresh_tick += 1;
            }
        }
    });

    // Fetch processes (re-fetches when refresh_tick changes)
    let processes = use_resource(move || {
        let _tick = *refresh_tick.read(); // subscribe to refresh_tick
        async move { api::get_processes().await.unwrap_or_default() }
    });
    let procs_data = processes.read();

    rsx! {
        div { class: "flex items-center justify-between mb-6",
            div {
                h2 { class: "text-xl font-semibold text-white", "Process List" }
                p { class: "text-sm text-slate-400 mt-1", "Running k3rs processes across cluster nodes" }
            }

            div { class: "flex items-center gap-3",
                // Manual reload button
                button {
                    class: "flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium text-slate-300 bg-slate-800 border border-slate-700 hover:bg-slate-700 hover:text-white transition-all active:scale-95",
                    onclick: move |_| refresh_tick += 1,
                    Icon { width: 14, height: 14, icon: LdRefreshCw }
                    span { "Reload" }
                }

                // Auto-reload toggle
                button {
                    class: if *auto_reload.read() {
                        "flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium text-emerald-400 bg-emerald-500/10 border border-emerald-500/30 hover:bg-emerald-500/20 transition-all"
                    } else {
                        "flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium text-slate-500 bg-slate-800 border border-slate-700 hover:bg-slate-700 transition-all"
                    },
                    onclick: move |_| { let v = *auto_reload.read(); auto_reload.set(!v); },
                    Icon { width: 14, height: 14, icon: LdTimer }
                    span { "Auto" }
                }

                // Interval input
                div { class: "flex items-center gap-1.5",
                    input {
                        r#type: "number",
                        min: "1",
                        max: "60",
                        value: "{interval_secs}",
                        class: "w-14 bg-slate-800 border border-slate-700 rounded-md px-2 py-1.5 text-xs text-slate-300 text-center outline-none focus:border-blue-500 transition-colors",
                        onchange: move |evt| {
                            if let Ok(v) = evt.value().parse::<u64>() {
                                if (3..=60).contains(&v) {
                                    interval_secs.set(v);
                                } else {
                                    interval_secs.set(3);
                                }
                            }
                        },
                    }
                    span { class: "text-xs text-slate-500", "s" }
                }
            }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Node" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Process Name" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "CPU %" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Memory (RSS)" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "PID" }
                    }
                }
                tbody {
                    if let Some(procs) = procs_data.as_ref() {
                        if procs.is_empty() {
                            tr {
                                td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No k3rs processes found" }
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
                                    } else if p.cpu_percent > 20.0 {
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
