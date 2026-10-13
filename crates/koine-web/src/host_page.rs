//! `/host`: where this page's host is, and the bearer another app needs to
//! drive it (`docs/plan-koine-mobile.md` M-D2, M-D4).
//!
//! With a [`HostLink`](crate::HostLink) installed (a native app that linked
//! its host) the page shows the app id, the origin and the bearer with a Copy
//! button. Without one (the browser) it shows the window's origin and where
//! the token it holds came from, with the same paste field the status line
//! has.

use dioxus::prelude::*;

use crate::session::Session;
use crate::token;
use crate::{api, dom, host_link, HostLink, TokenField};

#[component]
pub fn HostPage() -> Element {
    match host_link() {
        Some(link) => rsx! { LinkedHost { link } },
        None => rsx! { BrowserHost {} },
    }
}

/// The linked host: app id, origin, bearer.
#[component]
fn LinkedHost(link: HostLink) -> Element {
    // Masked until asked for, or until a copy fails and the person has to
    // select it by hand.
    let mut revealed = use_signal(|| false);
    let mut copied = use_signal(|| None::<bool>);
    let bearer = link.token.clone();
    let copy_value = bearer.clone();
    rsx! {
        section { class: "kw-host",
            h1 { "Host" }
            dl {
                dt { "app" }
                dd { "{link.app_id}" }
                dt { "origin" }
                dd { code { class: "kw-secret", "{link.origin}" } }
                dt { "bearer" }
                dd {
                    match bearer.as_ref() {
                        None => rsx! { span { class: "kw-note", "none was handed over" } },
                        Some(value) if revealed() => rsx! {
                            code { class: "kw-secret", "{value}" }
                        },
                        Some(value) => rsx! {
                            input {
                                r#type: "password",
                                readonly: true,
                                value: "{value}",
                                "aria-label": "bearer",
                            }
                        },
                    }
                }
            }
            if bearer.is_some() {
                p {
                    button {
                        onclick: move |_| {
                            let value = copy_value.clone().unwrap_or_default();
                            spawn(async move {
                                let ok = dom::copy_to_clipboard(&value).await;
                                copied.set(Some(ok));
                                if !ok {
                                    revealed.set(true);
                                }
                            });
                        },
                        "Copy"
                    }
                    " "
                    button { onclick: move |_| revealed.set(!revealed()),
                        if revealed() { "Hide" } else { "Show" }
                    }
                    match copied() {
                        Some(true) => rsx! { span { class: "kw-note", " copied" } },
                        Some(false) => rsx! {
                            span { class: "kw-note", " could not copy: select the value above" }
                        },
                        None => rsx! {},
                    }
                }
            }
            p { class: "kw-note",
                "Another app on this device dials the origin with "
                code { "Authorization: Bearer <bearer>" }
                "; a request that only reads needs no bearer."
            }
            HostChecks {}
        }
    }
}

/// The app's startup notes and a live `GET /api/status` from this page's
/// own transport, each with its whole error chain: on a phone, this is the
/// only place that says which layer fails.
#[component]
fn HostChecks() -> Element {
    let notes = crate::notes();
    let mut result = use_signal(|| None::<Result<String, String>>);
    let mut runs = use_signal(|| 0u32);
    use_effect(move || {
        let _ = runs();
        spawn(async move {
            let answer = match api::ping().await {
                Ok(value) => Ok(value.to_string()),
                Err(error) => Err(format!("{}: {}", error.code, error.message)),
            };
            result.set(Some(answer));
        });
    });
    rsx! {
        h2 { "Startup checks" }
        if notes.is_empty() {
            p { class: "kw-note", "none recorded" }
        } else {
            ul { class: "kw-checks",
                for note in notes.iter() {
                    li { "{note}" }
                }
            }
        }
        h2 { "This page's transport" }
        p {
            button { onclick: move |_| runs.set(runs() + 1), "Check now" }
            " "
            match result() {
                None => rsx! { span { class: "kw-note", "checking…" } },
                Some(Ok(status)) => rsx! { code { "{status}" } },
                Some(Err(error)) => rsx! { code { class: "kw-refusal", "{error}" } },
            }
        }
    }
}

/// No link: the window's origin and the token's source, with the paste field.
#[component]
fn BrowserHost() -> Element {
    let session = use_context::<Session>();
    let held = session.token;
    let origin = api::origin();
    // Re-read when the token changes (pasted or forgotten) so the source is
    // the current one.
    let source = use_resource(move || async move {
        let _ = held.read().is_some();
        token::load_sourced().await.map(|(_, source)| source)
    });
    rsx! {
        section { class: "kw-host",
            h1 { "Host" }
            dl {
                dt { "origin" }
                dd { code { class: "kw-secret", "{origin}" } }
                dt { "token" }
                dd {
                    match &*source.read() {
                        None => rsx! { span { class: "kw-note", "reading…" } },
                        Some(Some(source)) => rsx! { "{source.label()}" },
                        Some(None) => rsx! { "none: the page is read-only" },
                    }
                }
            }
            if held.read().is_none() {
                p { TokenField {} }
            }
            p { class: "kw-note",
                "This page is served by its host, so its origin is the window's. "
                "A token reaches it from the page's meta tag or the field above, and stays in this tab."
            }
        }
    }
}
