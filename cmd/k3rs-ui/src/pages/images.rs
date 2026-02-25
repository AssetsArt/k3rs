use crate::api;
use dioxus::prelude::*;

#[component]
pub fn Images() -> Element {
    let images = use_resource(move || async move { api::get_images().await.unwrap_or_default() });
    let imgs_data = images.read();

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Images" }
            p { class: "text-sm text-slate-400 mt-1", "OCI container images across all nodes" }
        }

        div { class: "bg-slate-900 border border-slate-800 rounded-xl overflow-hidden",
            table { class: "w-full",
                thead {
                    tr { class: "border-b border-slate-800",
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Node" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Image ID" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Arch" }
                        th { class: "text-left px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "OS" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Layers" }
                        th { class: "text-right px-5 py-2.5 text-[11px] uppercase tracking-wider text-slate-500 font-semibold", "Size" }
                    }
                }
                tbody {
                    if let Some(imgs) = imgs_data.as_ref() {
                        if imgs.is_empty() {
                            tr {
                                td { colspan: "6", class: "text-center py-16 text-slate-500 text-sm", "No cached images" }
                            }
                        } else {
                            for img in imgs.iter() {
                                tr { class: "border-b border-slate-800/50 hover:bg-slate-800/30 transition-colors",
                                    td { class: "px-5 py-2.5 text-sm text-emerald-300 font-medium",
                                        "{img.node_name}"
                                    }
                                    td { class: "px-5 py-2.5 text-sm text-white font-mono",
                                        "{img.id}"
                                    }
                                    td { class: "px-5 py-2.5 text-sm text-slate-300",
                                        span { class: "px-2 py-0.5 rounded-full text-[11px] font-semibold bg-violet-500/15 text-violet-300",
                                            "{img.architecture}"
                                        }
                                    }
                                    td { class: "px-5 py-2.5 text-sm text-slate-300",
                                        "{img.os}"
                                    }
                                    td { class: "text-right px-5 py-2.5 text-sm text-slate-400",
                                        "{img.layers}"
                                    }
                                    td { class: "text-right px-5 py-2.5 text-sm text-blue-300 font-medium",
                                        "{img.size_human}"
                                    }
                                }
                            }
                        }
                    } else {
                        tr {
                            td { colspan: "6", class: "text-center py-16 text-slate-500 text-sm",
                                "Loading…"
                            }
                        }
                    }
                }
            }
        }
    }
}
