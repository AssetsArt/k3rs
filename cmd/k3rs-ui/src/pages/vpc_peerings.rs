use crate::api;
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

#[component]
pub fn VpcPeerings() -> Element {
    let peerings =
        use_resource(|| async move { api::get_vpc_peerings().await.unwrap_or_default() });
    let data = peerings.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "VPC Peerings" }
            p { class: "text-sm text-slate-400 mt-1", "Cross-VPC connectivity rules" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "VPC A" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "VPC B" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Direction" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Status" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Created" }
                    }
                }
                tbody {
                    if let Some(peerings) = data.as_ref() {
                        if peerings.is_empty() {
                            tr { td { colspan: "6", class: "text-center py-16 text-slate-500 text-sm", "No VPC peerings found" } }
                        } else {
                            for p in peerings.iter() {
                                {
                                    let dir_cls = match p.direction.as_str() {
                                        "Bidirectional" => "bg-violet-500/10 text-violet-400 border border-violet-500/20",
                                        _ => "bg-blue-500/10 text-blue-400 border border-blue-500/20",
                                    };
                                    rsx! {
                                        tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                            td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{p.name}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-400", "{p.vpc_a}" }
                                            td { class: "px-5 py-3 text-xs font-mono text-slate-400", "{p.vpc_b}" }
                                            td { class: "px-5 py-3",
                                                span { class: "inline-block px-2.5 py-0.5 rounded-full text-[11px] font-medium {dir_cls}", "{p.direction}" }
                                            }
                                            td { class: "px-5 py-3", StatusBadge { status: p.status.clone() } }
                                            td { class: "px-5 py-3 text-xs text-slate-500", "{p.created_at}" }
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
