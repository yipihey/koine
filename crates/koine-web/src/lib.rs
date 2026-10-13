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
///
/// Above 700px this is the desktop page. At or below it (a phone,
/// plan-koine-tour T-D4; `koine.css` and the layout page use the same
/// width) the nav wraps, every control is full width and at
/// least a finger tall, and the status line's field no longer asks for 14rem.
/// `-webkit-text-size-adjust: 100%` keeps iOS from inflating the text when
/// the page is rotated, and a control's text is never under 16px on a touch
/// screen, the size below which iOS zooms into a focused input.
const PAGE_CSS: &str = r#"
html { -webkit-text-size-adjust: 100%; text-size-adjust: 100%; }
body { margin: 0; font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  padding: env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left); }
.kw-status { display: flex; flex-wrap: wrap; gap: 4px 16px; align-items: center;
  padding: 6px 16px; font-size: 0.85rem; border-bottom: 1px solid #8884; }
.kw-status .kw-refusal { color: #dc2626; }
.kw-status input { font: inherit; padding: 1px 6px; min-width: 14rem; }
.kw-nav { display: flex; flex-wrap: wrap; gap: 16px; padding: 8px 16px; border-bottom: 1px solid #8884; }
.kw-main { padding: 16px; }
.kw-surface:focus { outline: none; }
.kw-note { opacity: 0.7; font-size: 0.9rem; }
.kw-surfaces { margin: 0; padding: 0; list-style: none; }
.kw-surfaces li { border-bottom: 1px solid #8884; }
.kw-surfaces a { display: block; padding: 8px 4px; text-decoration: none; color: inherit; }
.kw-surfaces a:hover { background: #8882; }
.kw-surfaces strong { display: block; font-weight: 600; }
.kw-surfaces small { display: block; opacity: 0.7; overflow-wrap: anywhere; }
.kw-host dl { display: grid; grid-template-columns: max-content 1fr; gap: 6px 16px; }
.kw-host dd { margin: 0; overflow-wrap: anywhere; }
.kw-host input { font: inherit; min-width: 18rem; max-width: 100%; }
.kw-secret { user-select: all; -webkit-user-select: all; }
@media (max-width: 700px) {
  .kw-status, .kw-nav, .kw-main { padding-left: 12px; padding-right: 12px; }
  .kw-status { gap: 6px 12px; }
  .kw-status input, .kw-host input { min-width: 0; width: 100%; box-sizing: border-box; }
  .kw-nav { gap: 4px 8px; }
  .kw-nav a { padding: 10px 8px; min-height: 44px; box-sizing: border-box;
    display: inline-flex; align-items: center; }
  .kw-host dl { grid-template-columns: 1fr; gap: 2px 0; }
  .kw-host dt { margin-top: 8px; }
  .kw-surfaces a { min-height: 44px; box-sizing: border-box; padding: 10px 4px; }
}
@media (max-width: 700px), (pointer: coarse) {
  .kw-status input, .kw-host input, .kw-status button, .kw-host button { font-size: max(1em, 16px); }
  .kw-status button, .kw-host button { min-height: 44px; min-width: 44px; padding: 8px 12px; }
  a, button { touch-action: manipulation; }
}
"#;

/// The `<head>` the native build (a WKWebView under `dioxus-desktop`) adds
/// to the index it serves. dioxus-desktop's own index already carries a
/// viewport meta (`width=device-width, initial-scale=1.0, maximum-scale=1.0,
/// user-scalable=no`); this one comes after it, before `</head>`, so WebKit
/// takes it as the viewport (the last viewport meta processed wins), and it
/// adds `viewport-fit=cover` so the page may run under the notch and the
/// home bar, where `body`'s safe-area padding keeps the text out from under
/// them. The browser bundle's `index.html` is `dx`'s, which carries
/// `width=device-width, initial-scale=1` of its own.
#[cfg(any(feature = "desktop", feature = "mobile"))]
const NATIVE_HEAD: &str =
    r#"<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">"#;

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

/// Lines a native app adds before [`launch`], shown on `/host` under
/// "startup checks": what its own probes of the host found (the demo's
/// plain-socket GET of `/api/status`), so a phone with no console still
/// says what happened.
static NOTES: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Add a startup note (see [`notes`]).
pub fn add_note(note: impl Into<String>) {
    NOTES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(note.into());
}

/// The startup notes, in the order added.
pub fn notes() -> Vec<String> {
    NOTES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

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
    #[cfg(any(feature = "desktop", feature = "mobile"))]
    {
        // `dioxus::mobile` and `dioxus::desktop` are both `dioxus_desktop`;
        // the WebView's config is the same type under either feature.
        #[cfg(feature = "desktop")]
        use dioxus::desktop::Config;
        #[cfg(all(feature = "mobile", not(feature = "desktop")))]
        use dioxus::mobile::Config;
        let config = Config::new().with_custom_head(NATIVE_HEAD.to_string());
        dioxus::LaunchBuilder::new().with_cfg(config).launch(App);
    }
    #[cfg(all(
        target_arch = "wasm32",
        not(any(feature = "desktop", feature = "mobile"))
    ))]
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

/// How often the home page lists the surfaces again while it is open: a
/// surface an agent (the tour's concierge) just made shows up without a
/// reload. The list has no wait route; the surface routes do.
const LIST_POLL_MS: u32 = 3_000;

/// What the home page says when the app has no surfaces.
pub const NO_SURFACES: &str = "No surfaces yet. On the demo, the tour seeds five at launch.";

/// `/`: the app's surfaces, and the way to the other pages.
///
/// The list is fetched on mount and every [`LIST_POLL_MS`] while the tab is
/// visible (the layout page's loop, with the same flag). The first answer,
/// and the first after a failure, clear the status line; a later quiet poll
/// does not erase a refusal another page earned.
#[component]
fn Home() -> Element {
    let session = use_context::<Session>();
    let mut surfaces: Signal<Option<Result<api::SurfaceListResult, api::ApiError>>> =
        use_signal(|| None);
    let alive = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(true)));
    use_drop({
        let alive = alive.clone();
        move || alive.set(false)
    });
    use_future(move || {
        let alive = alive.clone();
        async move {
            let mut failed = true;
            while alive.get() {
                if !dom::page_hidden().await {
                    let answer = api::list().await;
                    if !alive.get() {
                        break;
                    }
                    match &answer {
                        Ok(_) => {
                            if failed {
                                session.clear();
                                failed = false;
                            }
                        }
                        Err(error) => {
                            session.refuse(error.clone());
                            failed = true;
                        }
                    }
                    if surfaces.peek().as_ref() != Some(&answer) {
                        surfaces.set(Some(answer));
                    }
                }
                dom::sleep(LIST_POLL_MS).await;
            }
        }
    });
    rsx! {
        h1 { "Surfaces" }
        match &*surfaces.read() {
            None => rsx! { p { class: "kw-note", "listing…" } },
            Some(Err(error)) => rsx! { p { "could not list surfaces ({error.code})" } },
            Some(Ok(list)) if list.surfaces.is_empty() => {
                rsx! { p { class: "kw-note", "{NO_SURFACES}" } }
            }
            Some(Ok(list)) => rsx! {
                ul { class: "kw-surfaces",
                    for surface in list.surfaces.iter() {
                        li { key: "{surface.id}",
                            Link { to: Route::SurfacePage { id: surface.id.clone() },
                                strong { "{surface.title()}" }
                                if let Some(description) = surface.description() {
                                    small { "{description}" }
                                } else {
                                    small { "{surface.id}" }
                                }
                            }
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
