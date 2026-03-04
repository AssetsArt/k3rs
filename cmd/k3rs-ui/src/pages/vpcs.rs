use crate::api;
use dioxus::prelude::*;

use super::dashboard::StatusBadge;

#[component]
pub fn Vpcs() -> Element {
    let vpcs = use_resource(|| async move { api::get_vpcs().await.unwrap_or_default() });
    let data = vpcs.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "VPCs" }
            p { class: "text-sm text-slate-400 mt-1", "Virtual Private Clouds for pod network isolation" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "VPC ID" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Name" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Status" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "CIDR" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Created" }
                    }
                }
                tbody {
                    if let Some(vpcs) = data.as_ref() {
                        if vpcs.is_empty() {
                            tr { td { colspan: "5", class: "text-center py-16 text-slate-500 text-sm", "No VPCs found" } }
                        } else {
                            for vpc in vpcs.iter() {
                                tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                    td { class: "px-5 py-3 text-xs font-mono text-slate-400", "{vpc.vpc_id}" }
                                    td { class: "px-5 py-3 text-sm text-slate-300 font-medium", "{vpc.name}" }
                                    td { class: "px-5 py-3", StatusBadge { status: vpc.status.clone() } }
                                    td { class: "px-5 py-3 text-xs font-mono text-slate-400", "{vpc.ipv4_cidr}" }
                                    td { class: "px-5 py-3 text-xs text-slate-500", "{vpc.created_at}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
