//! `koine-web`: the koine web frontend as a page.
//!
//! It draws any surface of a running impress app, live, and the style editor
//! for its own sheet. The mapping from a `RenderTree` to elements is
//! `koine-dioxus`; this crate is the transport ([`api`]), the token
//! ([`token`]), the keys ([`keys`]), the router, and the pages that use
//! them. It talks to the app on the page's own origin: there are no CORS
//! headers, so the page is served by the app at `/ui/` or by `dx serve` with
//! its proxy. See `README.md` and `docs/plan-koine-web.md` D6 to D9.
//!
//! The crate is also a library: an app that links its own host (an iPhone app,
//! `docs/plan-koine-mobile.md`) installs a [`HostLink`] and calls [`launch`],
//! and the page dials that host instead of the window's origin.

mod api;
mod dom;
mod host_page;
mod keys;
mod layout_page;
mod session;
mod style_page;
mod surface_page;
mod token;

use std::sync::RwLock;

use dioxus::prelude::*;

use crate::host_page::HostPage;
use crate::layout_page::LayoutPage;
use crate::session::Session;
use crate::style_page::StylePage;
use crate::surface_page::SurfacePage;

/// The page's own chrome. The surface is `koine_dioxus::STYLESHEET`.
const PAGE_CSS: &str = r#"
body { margin: 0; font-family: system-ui, -apple-system, "Segoe UI", sans-serif; }
.kw-status { display: flex; flex-wrap: wrap; gap: 4px 16px; align-items: center;
  padding: 6px 16px; font-size: 0.85rem; border-bottom: 1px solid #8884; }
.kw-status .kw-refusal { color: #dc2626; }
.kw-status input { font: inherit; padding: 1px 6px; min-width: 14rem; }
.kw-nav { display: flex; gap: 16px; padding: 8px 16px; border-bottom: 1px solid #8884; }
.kw-main { padding: 16px; }
.kw-surface:focus { outline: none; }
.kw-note { opacity: 0.7; font-size: 0.9rem; }
.kw-host dl { display: grid; grid-template-columns: max-content 1fr; gap: 6px 16px; }
.kw-host dd { margin: 0; overflow-wrap: anywhere; }
.kw-host input { font: inherit; min-width: 18rem; max-width: 100%; }
.kw-secret { user-select: all; -webkit-user-select: all; }
"#;

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[layout(Shell)]
    #[route("/")]
    Home {},
    #[route("/surface/:id")]
    SurfacePage { id: String },
    #[route("/layout")]
    LayoutPage {},
    #[route("/style")]
    StylePage {},
    #[route("/host")]
    HostPage {},
}

/// Where the page's host is, handed over in-process (M-D2).
///
/// A browser build never has one: its host is the origin that served it. A
/// native build that links `koine-host` starts the host, learns its loopback
/// URL and bearer, and installs them before [`launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLink {
    /// The host's origin, for example `http://127.0.0.1:49152`.
    pub origin: String,
    /// The host's loopback bearer. It is held in memory only: the page never
    /// writes it to `sessionStorage` or a URL.
    pub token: Option<String>,
    /// The app's id, shown on `/host`.
    pub app_id: String,
}

static LINK: RwLock<Option<HostLink>> = RwLock::new(None);

/// Install the link once, before [`launch`]. A second call replaces the first
/// (tests); the page reads it on every request.
pub fn install_link(link: HostLink) {
    *LINK
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(link);
}

/// The installed link, if any.
pub fn host_link() -> Option<HostLink> {
    LINK.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Forget the link (tests).
#[cfg(test)]
pub(crate) fn clear_link() {
    *LINK
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Tests that touch the process-wide link take this first: the link is global
/// and the test harness runs tests in parallel.
#[cfg(test)]
pub(crate) static LINK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Launch the page: `dioxus::launch(App)` in a browser (wasm32) or under the
/// `desktop` or `mobile` feature. A native build with none of them (`cargo
/// test`, `cargo check`) has no window to open, so it prints the hint and
/// returns: the pure modules are what it compiles and tests.
pub fn launch() {
    #[cfg(any(target_arch = "wasm32", feature = "desktop", feature = "mobile"))]
    dioxus::launch(App);
    #[cfg(not(any(target_arch = "wasm32", feature = "desktop", feature = "mobile")))]
    {
        eprintln!("koine-web draws in a browser: `dx serve`, or `dx build --platform web`");
        let _ = App;
    }
}

#[component]
fn App() -> Element {
    let session = use_context_provider(Session::new);
    let mut token = session.token;
    use_future(move || async move {
        token.set(token::load().await);
    });
    rsx! {
        style { dangerous_inner_html: koine_dioxus::STYLESHEET }
        style { dangerous_inner_html: PAGE_CSS }
        Router::<Route> {}
    }
}

/// The frame of every page: the status line, the navigation, the page.
#[component]
fn Shell() -> Element {
    rsx! {
        StatusLine {}
        nav { class: "kw-nav",
            Link { to: Route::Home {}, "surfaces" }
            Link { to: Route::LayoutPage {}, "layout" }
            Link { to: Route::StylePage {}, "style" }
            Link { to: Route::HostPage {}, "host" }
        }
        main { class: "kw-main", Outlet::<Route> {} }
    }
}

/// App origin, whether a token is held, the last refusal.
#[component]
fn StatusLine() -> Element {
    let session = use_context::<Session>();
    let mut held = session.token;
    let refusal = session.refusal;
    let origin = api::origin();
    let linked_token = host_link().is_some_and(|link| link.token.is_some());
    rsx! {
        div { class: "kw-status", role: "status",
            span { "app: {origin}" }
            if held.read().is_some() {
                span { "token held" }
                // A linked host's token is the program's own, not the tab's:
                // there is nothing stored to forget.
                if !linked_token {
                    button {
                        onclick: move |_| {
                            held.set(None);
                            spawn(async move { token::forget().await });
                        },
                        "forget"
                    }
                }
            } else {
                span { "read-only: no token" }
                TokenField {}
            }
            if let Some(error) = refusal.read().as_ref() {
                span { class: "kw-refusal", "{error.code}: {error.message}" }
            }
        }
    }
}

/// The field a person pastes the loopback token into: kept in
/// `sessionStorage` for this tab (plan D8), never in the URL.
#[component]
pub(crate) fn TokenField() -> Element {
    let session = use_context::<Session>();
    let mut held = session.token;
    rsx! {
        input {
            r#type: "password",
            placeholder: "paste loopback token",
            "aria-label": "loopback token",
            onchange: move |event| {
                if let Some(pasted) = token::Token::new(&event.value()) {
                    held.set(Some(pasted.clone()));
                    spawn(async move { token::store(&pasted).await });
                }
            },
        }
    }
}

/// `/`: the app's surfaces, and the way to the other two pages.
#[component]
fn Home() -> Element {
    let session = use_context::<Session>();
    let surfaces = use_resource(api::list);
    use_effect(move || match &*surfaces.read() {
        Some(Err(error)) => session.refuse(error.clone()),
        Some(Ok(_)) => session.clear(),
        None => {}
    });
    rsx! {
        h1 { "Surfaces" }
        match &*surfaces.read() {
            None => rsx! { p { class: "kw-note", "listing…" } },
            Some(Err(error)) => rsx! { p { "could not list surfaces ({error.code})" } },
            Some(Ok(list)) if list.surfaces.is_empty() => {
                rsx! { p { class: "kw-note", "the app has no surfaces yet" } }
            }
            Some(Ok(list)) => rsx! {
                ul {
                    for surface in list.surfaces.iter() {
                        li { key: "{surface.id}",
                            Link { to: Route::SurfacePage { id: surface.id.clone() }, "{surface.name}" }
                            span { class: "kw-note", " {surface.id}" }
                        }
                    }
                }
            },
        }
        p {
            Link { to: Route::LayoutPage {}, "layout" }
            " · "
            Link { to: Route::StylePage {}, "style" }
            " · "
            Link { to: Route::HostPage {}, "host" }
        }
    }
}
