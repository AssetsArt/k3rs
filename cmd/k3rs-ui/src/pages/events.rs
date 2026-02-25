use crate::WatchEvent;
use dioxus::prelude::*;

/// Events page — displays cluster events.
#[component]
pub fn Events() -> Element {
    let mut events = use_signal(Vec::<WatchEvent>::new);
    let mut loading = use_signal(|| true);

    // Note: events endpoint may not exist yet. Show empty state gracefully.
    use_effect(move || {
        spawn(async move {
            // Events will be populated when the watch API returns data
            events.set(vec![]);
            loading.set(false);
        });
    });

    rsx! {
        div { class: "page-header",
            h2 { "Events" }
            p { "Cluster event stream" }
        }

        if *loading.read() {
            div { class: "empty-state",
                div { class: "icon", "⏳" }
                p { "Loading events..." }
            }
        } else if events.read().is_empty() {
            div { class: "empty-state",
                div { class: "icon", "📋" }
                p { "No events yet. Events will appear here as resources are created, updated, or deleted." }
            }
        } else {
            div { class: "event-list",
                for evt in events.read().iter().rev() {
                    div { class: "event-item",
                        span {
                            class: format!("event-type {}", evt.event_type.to_lowercase()),
                            "{evt.event_type}"
                        }
                        span { class: "event-key", "{evt.key}" }
                        span { class: "event-seq", "#{evt.seq}" }
                    }
                }
            }
        }
    }
}
