//! `koine-web`: the koine web frontend as a page.
//!
//! It draws any surface of a running impress app, live, and the style editor
//! for its own sheet. The mapping from a `RenderTree` to elements is
//! `koine-dioxus`; this crate is the transport ([`api`]), the token
//! ([`token`]), the keys ([`keys`]), the router, and the two pages that use
//! them. It talks to the app on the page's own origin: there are no CORS
//! headers, so the page is served by the app at `/ui/` or by `dx serve` with
//! its proxy. See `README.md` and `docs/plan-koine-web.md` D6 to D9.

mod api;
mod dom;
mod keys;
mod session;
mod style_page;
mod surface_page;
mod token;

use dioxus::prelude::*;

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
}

fn main() {
    launch();
}

#[cfg(any(target_arch = "wasm32", feature = "desktop"))]
fn launch() {
    dioxus::launch(App);
}

/// A native build with no renderer (`cargo test`, `cargo check`) has no
/// window to open: the pure modules are what it compiles and tests.
#[cfg(not(any(target_arch = "wasm32", feature = "desktop")))]
fn launch() {
    eprintln!("koine-web draws in a browser: `dx serve`, or `dx build --platform web`");
    let _ = App;
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
    rsx! {
        div { class: "kw-status", role: "status",
            span { "app: {origin}" }
            if held.read().is_some() {
                span { "token held" }
                button {
                    onclick: move |_| {
                        held.set(None);
                        spawn(async move { token::forget().await });
                    },
                    "forget"
                }
            } else {
                span { "read-only: no token" }
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
            if let Some(error) = refusal.read().as_ref() {
                span { class: "kw-refusal", "{error.code}: {error.message}" }
            }
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
        }
    }
}

/// `/layout`: the window tree (plan D7), a later package.
#[component]
fn LayoutPage() -> Element {
    rsx! { p { "layout: not yet" } }
}
