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

/// The page's own chrome (docs/plan-koine-theme.md). The surface, and the
/// theme's tokens and fonts, are `koine_dioxus::STYLESHEET`.
///
/// Above 700px this is a desktop window: a toolbar across the top with the
/// pages as icon-and-label items (TH-D2), a 22px title, 13px text.
/// At or below it, or with a coarse pointer (a phone, plan-koine-tour T-D4;
/// `koine.css` and the layout page use the same width), the pages are
/// a tab bar pinned over the home indicator, the title is a large title,
/// the home list and the host's pairs are grouped inset cards (TH-D3), and
/// `main` is padded so nothing hides under the bar.
/// `-webkit-text-size-adjust: 100%` keeps iOS from inflating the text when
/// the page is rotated, and a control's text is never under 16px on a touch
/// screen, the size below which iOS zooms into a focused input.
const PAGE_CSS: &str = r#"
html { -webkit-text-size-adjust: 100%; text-size-adjust: 100%; }
/* The top safe-area inset is not body padding: that scrolls away with the
   page, and on a desktop the sticky toolbar slides over it. It belongs to
   whatever is at the top: the toolbar here, `.kw-main` on a phone (below). */
body { margin: 0; min-height: 100vh;
  padding: 0 env(safe-area-inset-right) 0 env(safe-area-inset-left); }
a { color: var(--koine-color-accent); text-decoration: none; }
.kw-main { max-width: 980px; margin: 0 auto; padding: 8px 20px 32px; box-sizing: border-box; }
.kw-main h1 { margin: 10px 0 12px; font-size: 22px; font-weight: 700; letter-spacing: -0.26px; line-height: 1.2; }
.kw-main h2 { margin: 20px 0 6px; padding: 0 16px; font-size: 13px; font-weight: 400; line-height: 1.3;
  color: var(--koine-color-secondary); text-transform: uppercase; letter-spacing: -0.08px; }
.kw-main p { margin: 10px 0; }
.kw-note { color: var(--koine-color-secondary); font-size: 13px; overflow-wrap: anywhere; }
.kw-note code { font-family: var(--koine-font-mono); }
.kw-surface:focus { outline: none; }
/* The way back from a surface: a back link, then the id, cut to the line. */
.kw-crumb { display: flex; align-items: center; gap: 8px; min-width: 0; }
.kw-crumb a { display: inline-flex; align-items: center; gap: 2px; flex: none; }
.kw-crumb a::before { content: ""; width: 8px; height: 8px; margin-right: 2px;
  border-left: 2px solid currentColor; border-bottom: 2px solid currentColor; transform: rotate(45deg); }
.kw-crumb-id { flex: 1 1 auto; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.kw-crumb-id::before { content: "/ "; }
.kw-refusal { color: var(--koine-color-error); }

/* The navigation: a toolbar on a desktop, a tab bar on a phone (below). */
.kw-nav { position: sticky; top: 0; z-index: 20; display: flex; align-items: center; gap: 2px;
  min-height: calc(52px + env(safe-area-inset-top, 0px)); box-sizing: border-box;
  padding: calc(4px + env(safe-area-inset-top, 0px)) 12px 4px;
  background: var(--koine-bg-bar); -webkit-backdrop-filter: saturate(180%) blur(20px); backdrop-filter: saturate(180%) blur(20px);
  border-bottom: var(--koine-hairline) solid var(--koine-separator);
  user-select: none; -webkit-user-select: none; }
.kw-nav-title { margin: 0 14px 0 4px; font-size: 13px; font-weight: 600; }
.kw-tab { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px;
  min-width: 58px; padding: 3px 8px; border-radius: 6px; color: var(--koine-color-secondary);
  font-size: 11px; line-height: 1.2; text-decoration: none; touch-action: manipulation; }
.kw-tab svg { width: 22px; height: 22px; display: block; }
.kw-tab-active { color: var(--koine-color-accent); }
.kw-tab:active { opacity: 0.6; }
@media (hover: hover) { .kw-tab:hover { background: var(--koine-bg-fill); } }

/* The refusal banner: only drawn while there is a refusal. */
.kw-banner { padding: 10px 20px; font-weight: 500; overflow-wrap: anywhere;
  color: var(--koine-color-error); background: color-mix(in srgb, var(--koine-color-error) 12%, var(--koine-bg-grouped)); }

/* The home list: a grouped inset list, a chevron on every row. */
.kw-surfaces { margin: 0; padding: 0; list-style: none; background: var(--koine-bg-card); border-radius: var(--koine-radius-card); }
.kw-surfaces li { position: relative; }
.kw-surfaces li + li::before { content: ""; position: absolute; top: 0; left: 16px; right: 0;
  height: var(--koine-hairline); background: var(--koine-separator); pointer-events: none; }
.kw-surfaces a { display: grid; grid-template-columns: minmax(0, 1fr) 10px; column-gap: 12px; align-items: center;
  min-height: 36px; padding: 7px 16px; box-sizing: border-box; color: inherit; touch-action: manipulation; }
.kw-surfaces a::after { content: ""; grid-column: 2; grid-row: 1 / span 2; width: 8px; height: 8px;
  border-right: 2px solid var(--koine-color-tertiary); border-top: 2px solid var(--koine-color-tertiary);
  transform: rotate(45deg); margin-right: 3px; }
.kw-surfaces li:first-child a { border-top-left-radius: var(--koine-radius-card); border-top-right-radius: var(--koine-radius-card); }
.kw-surfaces li:last-child a { border-bottom-left-radius: var(--koine-radius-card); border-bottom-right-radius: var(--koine-radius-card); }
.kw-surfaces a:active { background: var(--koine-bg-pressed); }
@media (hover: hover) { .kw-surfaces a:hover { background: color-mix(in srgb, var(--koine-color-accent) 6%, transparent); } }
.kw-surfaces strong { display: block; font-weight: 400; min-width: 0; overflow-wrap: anywhere; }
.kw-surfaces small { display: block; font-size: 12px; color: var(--koine-color-secondary); min-width: 0;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

/* The host page: its pairs as a grouped card, its buttons as pills, its
   code as a card that wraps. */
.kw-host dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 0; margin: 0;
  background: var(--koine-bg-card); border-radius: var(--koine-radius-card); }
.kw-host dt, .kw-host dd { display: flex; align-items: center; min-height: 36px; box-sizing: border-box; padding: 6px 12px; }
.kw-host dt { padding-left: 16px; color: var(--koine-color-secondary); }
.kw-host dd { margin: 0; padding-right: 16px; min-width: 0; overflow-wrap: anywhere; }
.kw-host dt:nth-of-type(n + 2), .kw-host dt:nth-of-type(n + 2) + dd {
  border-top: var(--koine-hairline) solid var(--koine-separator); }
.kw-host dd code { font-family: var(--koine-font-mono); font-size: 0.95em; overflow-wrap: anywhere; }
.kw-host input { font: inherit; width: 100%; max-width: 28rem; min-height: 28px; padding: 6px 10px; box-sizing: border-box;
  border: var(--koine-hairline) solid var(--koine-separator); border-radius: var(--koine-radius-field);
  background: var(--koine-bg-field); color: var(--koine-color-primary); -webkit-appearance: none; appearance: none; }
.kw-host dd input { max-width: none; }
.kw-host input:focus-visible { outline: 2px solid color-mix(in srgb, var(--koine-color-accent) 60%, transparent); outline-offset: 0; }
.kw-host button { font: inherit; font-weight: 600; min-height: 28px; padding: 5px 14px; border: 0; border-radius: 999px;
  background: color-mix(in srgb, var(--koine-color-accent) 15%, transparent); color: var(--koine-color-accent);
  cursor: pointer; user-select: none; -webkit-user-select: none; touch-action: manipulation; }
.kw-host button:active { background: color-mix(in srgb, var(--koine-color-accent) 28%, transparent); }
.kw-code { margin: 10px 0; padding: 10px 16px; max-height: 40vh; overflow: auto; box-sizing: border-box;
  font-family: var(--koine-font-mono); font-size: 0.9em; white-space: pre-wrap; overflow-wrap: anywhere;
  background: var(--koine-bg-card); border-radius: var(--koine-radius-card); }
.kw-checks { margin: 0; padding: 0; list-style: none; background: var(--koine-bg-card); border-radius: var(--koine-radius-card); }
.kw-checks li { position: relative; padding: 8px 16px; min-height: 36px; box-sizing: border-box; display: flex; align-items: center; overflow-wrap: anywhere; }
.kw-checks li + li::before { content: ""; position: absolute; top: 0; left: 16px; right: 0;
  height: var(--koine-hairline); background: var(--koine-separator); }
.kw-secret { user-select: all; -webkit-user-select: all; }

@media (max-width: 700px) {
  .kw-main h1 { margin: 6px 0 14px; font-size: 34px; letter-spacing: 0.37px; line-height: 41px; }
  .kw-main h2 { margin: 24px 0 8px; }
  .kw-note { font-size: 15px; }
  .kw-surfaces a { min-height: 44px; padding: 9px 16px; }
  .kw-surfaces small { font-size: 15px; }
  .kw-host dl { grid-template-columns: minmax(0, 1fr); }
  .kw-host dt { padding: 10px 16px 0; min-height: 0; font-size: 13px; }
  .kw-host dd { padding: 2px 16px 10px; min-height: 0; }
  .kw-host dt:nth-of-type(n + 2) + dd { border-top: 0; }
  .kw-host input { max-width: none; }
  .kw-checks li { min-height: 44px; }
  .kw-host dt, .kw-host dd { display: block; }
}

/* A phone, or a finger: the tab bar, pinned over the home indicator. */
@media (max-width: 700px), (pointer: coarse) {
  /* The large title sits 8px below the status bar, on every page: the
     inset is the top padding of the page itself, and its first element
     brings no margin of its own. A strip the colour of the bars covers the
     status bar, so a scrolled page never draws under the clock. */
  .kw-main { padding: calc(env(safe-area-inset-top, 0px) + 8px) 16px calc(49px + env(safe-area-inset-bottom) + 24px); }
  .kw-main h1, .kw-main .kw-crumb { margin-top: 0; }
  body::before { content: ""; position: fixed; top: 0; left: 0; right: 0; z-index: 21; pointer-events: none;
    height: env(safe-area-inset-top, 0px); background: var(--koine-bg-bar);
    -webkit-backdrop-filter: saturate(180%) blur(20px); backdrop-filter: saturate(180%) blur(20px); }
  .kw-nav { position: fixed; top: auto; bottom: 0; left: 0; right: 0; z-index: 20;
    min-height: 0; height: calc(49px + env(safe-area-inset-bottom)); padding: 0 0 env(safe-area-inset-bottom);
    justify-content: space-around; gap: 0; border-bottom: 0;
    border-top: var(--koine-hairline) solid var(--koine-separator); }
  .kw-nav-title { display: none; }
  .kw-tab { flex: 1 1 0; min-width: 0; height: 49px; padding: 6px 0 0; gap: 2px; border-radius: 0;
    justify-content: flex-start; font-size: 10px; font-weight: 500; }
  .kw-tab svg { width: 26px; height: 26px; }
  .kw-banner { position: fixed; left: 0; right: 0; bottom: calc(49px + env(safe-area-inset-bottom)); z-index: 19; padding: 11px 16px; }
  .kw-host input, .kw-host button { font-size: max(1em, 16px); }
  .kw-host button { min-height: 44px; min-width: 44px; padding: 8px 18px; }
  .kw-host p > button:only-child { width: 100%; }
  .kw-host input { min-height: 44px; padding: 10px 14px; }
  a, button { touch-action: manipulation; }
}
"#;

/// The `<head>` the native build (a WKWebView under `dioxus-desktop`) adds
/// to the index it serves. dioxus-desktop's own index already carries a
/// viewport meta (`width=device-width, initial-scale=1.0, maximum-scale=1.0,
/// user-scalable=no`); this one comes after it, before `</head>`, so WebKit
/// takes it as the viewport (the last viewport meta processed wins), and it
/// adds `viewport-fit=cover` so the page may run under the notch and the
/// home bar, where the page's safe-area padding (`PAGE_CSS`: the toolbar's
/// top, a phone's `.kw-main` top and tab bar bottom) keeps the text out from
/// under them. The browser bundle's `index.html` is `dx`'s, which carries
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
    // Which tabs the host can serve (A-D4): asked once here, and again by
    // the layout page's poll.
    use_future(move || async move { session.check_layout().await });
    rsx! {
        style { dangerous_inner_html: koine_dioxus::STYLESHEET }
        style { dangerous_inner_html: PAGE_CSS }
        Router::<Route> {}
    }
}

/// The frame of every page: the navigation, the refusal banner, the page.
#[component]
fn Shell() -> Element {
    rsx! {
        Nav {}
        RefusalBanner {}
        main { class: "kw-main", Outlet::<Route> {} }
    }
}

/// The pages a tab names (TH-D2). A surface page is the surfaces tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Surfaces,
    Layout,
    Style,
    Host,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Surfaces, Tab::Layout, Tab::Style, Tab::Host];

    fn of(route: &Route) -> Tab {
        match route {
            Route::Home {} | Route::SurfacePage { .. } => Tab::Surfaces,
            Route::LayoutPage {} => Tab::Layout,
            Route::StylePage {} => Tab::Style,
            Route::HostPage {} => Tab::Host,
        }
    }

    fn route(self) -> Route {
        match self {
            Tab::Surfaces => Route::Home {},
            Tab::Layout => Route::LayoutPage {},
            Tab::Style => Route::StylePage {},
            Tab::Host => Route::HostPage {},
        }
    }

    /// The tabs the nav shows (docs/plan-koine-affordances.md A-D4): a tab
    /// only when the host can serve its page.
    ///
    /// - "surfaces" and "host" always: every host lists surfaces, and the host
    ///   page is how a person reaches one that does not answer.
    /// - "layout" when `layout` ([`session::layout_tab`]: the host serves the
    ///   layout verbs and its layout has a tile the page can draw), or when it
    ///   is the page being shown, so the page in view keeps its tab.
    /// - "style" always: the editor is client-side. It edits the page's own
    ///   sheet through `koine::style::editor_apply` and calls no verb, so
    ///   every host can serve it.
    ///
    /// The routes stay registered whatever the nav shows.
    fn shown(layout: bool, current: Tab) -> Vec<Tab> {
        Tab::ALL
            .into_iter()
            .filter(|tab| *tab != Tab::Layout || layout || current == Tab::Layout)
            .collect()
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Surfaces => "surfaces",
            Tab::Layout => "layout",
            Tab::Style => "style",
            Tab::Host => "host",
        }
    }
}

/// The navigation: the pages the host can serve ([`Tab::shown`]) as
/// icon-and-label items, a toolbar across the top of a desktop window and a
/// tab bar pinned to the bottom of a phone (`PAGE_CSS` decides which; the
/// markup is one). Each item is a router `Link`, so it carries
/// `aria-current="page"` when it is the page.
#[component]
fn Nav() -> Element {
    let session = use_context::<Session>();
    let route = use_route::<Route>();
    let current = Tab::of(&route);
    let tabs = Tab::shown(*session.layout_tab.read(), current);
    rsx! {
        nav { class: "kw-nav", aria_label: "pages",
            span { class: "kw-nav-title", "koine" }
            for tab in tabs {
                Link {
                    key: "{tab.label()}",
                    to: tab.route(),
                    class: if tab == current { "kw-tab kw-tab-active" } else { "kw-tab" },
                    TabIcon { tab }
                    span { "{tab.label()}" }
                }
            }
        }
    }
}

/// The tab's icon, drawn inline: four squares, a window with a divider,
/// three slider lines, a pair of server boxes.
#[component]
fn TabIcon(tab: Tab) -> Element {
    rsx! {
        svg {
            view_box: "0 0 24 24",
            width: "24",
            height: "24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            "focusable": "false",
            match tab {
                Tab::Surfaces => rsx! {
                    rect { x: "3", y: "3", width: "7.5", height: "7.5", rx: "1.8" }
                    rect { x: "13.5", y: "3", width: "7.5", height: "7.5", rx: "1.8" }
                    rect { x: "3", y: "13.5", width: "7.5", height: "7.5", rx: "1.8" }
                    rect { x: "13.5", y: "13.5", width: "7.5", height: "7.5", rx: "1.8" }
                },
                Tab::Layout => rsx! {
                    rect { x: "3", y: "4", width: "18", height: "16", rx: "2.5" }
                    path { d: "M3 8.5h18M10 8.5V20" }
                },
                Tab::Style => rsx! {
                    path { d: "M4 7h16M4 12h16M4 17h16" }
                    circle { cx: "9", cy: "7", r: "2.2", fill: "var(--koine-bg-bar)" }
                    circle { cx: "15", cy: "12", r: "2.2", fill: "var(--koine-bg-bar)" }
                    circle { cx: "7", cy: "17", r: "2.2", fill: "var(--koine-bg-bar)" }
                },
                Tab::Host => rsx! {
                    rect { x: "3", y: "4", width: "18", height: "6.5", rx: "2" }
                    rect { x: "3", y: "13.5", width: "18", height: "6.5", rx: "2" }
                    path { d: "M7 7.25h.01M7 16.75h.01", stroke_width: "2.4" }
                },
            }
        }
    }
}

/// The last refusal, when there is one: a full-width banner under the
/// toolbar (a desktop) or above the tab bar (a phone). Drawn only while
/// `session.refusal` is `Some`; the origin and the token are on `/host`.
#[component]
fn RefusalBanner() -> Element {
    let session = use_context::<Session>();
    let refusal = session.refusal;
    rsx! {
        if let Some(error) = refusal.read().as_ref() {
            div { class: "kw-banner kw-refusal", role: "status", "{error.code}: {error.message}" }
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

/// `/`: the app's surfaces, as a grouped list; the nav is the way to the
/// other pages.
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
                                } else if !surface.tags.is_empty() {
                                    small { "{surface.tags.join(\", \")}" }
                                }
                            }
                        }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Tab;

    #[test]
    fn the_nav_shows_layout_only_when_the_host_can_serve_it() {
        assert_eq!(
            Tab::shown(false, Tab::Surfaces),
            vec![Tab::Surfaces, Tab::Style, Tab::Host]
        );
        assert_eq!(
            Tab::shown(true, Tab::Surfaces),
            vec![Tab::Surfaces, Tab::Layout, Tab::Style, Tab::Host]
        );
        // The page in view keeps its tab (the route is always registered).
        assert_eq!(Tab::shown(false, Tab::Layout), Tab::ALL.to_vec());
    }
}
