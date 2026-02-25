use crate::WatchEvent;
use dioxus::prelude::*;
use dioxus_free_icons::icons::ld_icons::*;
use dioxus_free_icons::Icon;

#[component]
pub fn Events() -> Element {
    let mut events = use_signal(Vec::<WatchEvent>::new);
    let mut loading = use_signal(|| true);

    use_effect(move || {
        spawn(async move {
            events.set(vec![]);
            loading.set(false);
        });
    });

    rsx! {
        div { class: "mb-6",
            h2 { class: "text-xl font-semibold text-white", "Events" }
            p { class: "text-sm text-slate-400 mt-1", "Cluster event stream" }
        }

        if *loading.read() {
            div { class: "flex flex-col items-center justify-center py-20 text-slate-500",
                Icon { width: 32, height: 32, icon: LdLoader }
                p { class: "mt-3 text-sm", "Loading events..." }
            }
        } else if events.read().is_empty() {
            div { class: "flex flex-col items-center justify-center py-20 text-slate-500",
                Icon { width: 32, height: 32, icon: LdInbox }
                p { class: "mt-3 text-sm", "No events yet" }
                p { class: "text-xs text-slate-600 mt-1", "Events will appear as resources change" }
            }
        } else {
            div { class: "space-y-2",
                for evt in events.read().iter().rev() {
                    div { class: "bg-slate-900 border border-slate-800 rounded-lg px-4 py-3 flex items-center gap-3 hover:border-slate-700 transition-colors",
                        span {
                            class: if evt.event_type.to_lowercase() == "put" {
                                "inline-block px-2 py-0.5 rounded text-[10px] font-semibold uppercase bg-emerald-500/10 text-emerald-400"
                            } else {
                                "inline-block px-2 py-0.5 rounded text-[10px] font-semibold uppercase bg-red-500/10 text-red-400"
                            },
                            "{evt.event_type}"
                        }
                        span { class: "text-xs font-mono text-slate-300 flex-1 truncate", "{evt.key}" }
                        span { class: "text-[11px] text-slate-600 shrink-0", "#{evt.seq}" }
                    }
                }
            }
        }
    }
}
