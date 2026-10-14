//! `/surface/:id`: one surface, live.
//!
//! On mount the page renders the surface; then one task long-polls
//! `…/wait` and renders again on an event or a timeout, so another process's
//! change (an agent's `surface_state_set`, a Swift pane's click) redraws the
//! page. A widget event is posted to `…/dispatch` and the answer's tree
//! replaces the page's. The page decides nothing about what an event means:
//! the app's `reduce` does.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;
use impress_surface::{Event, EventKind, RenderTree};
use koine::cursor::Cursor;
use koine_dioxus::outline::{self, MenuChoice, MenuKey, RowRef};
use koine_dioxus::{FileChosen, Surface};

use crate::api::{self, ApiError, WAIT_TIMEOUT_MS};
use crate::keys::{self, KeyAction};
use crate::session::Session;
use crate::{dom, Route};

/// How long to wait before asking again after the app did not answer.
const RETRY_MS: u32 = 2_000;

/// The route component. `SurfaceView` is keyed by id, so going from one
/// surface to another is a new page with a new wait loop, not a changed prop.
#[component]
pub fn SurfacePage(id: String) -> Element {
    rsx! {
        SurfaceView { key: "{id}", id: id.clone() }
    }
}

/// Everything one page instance reads and writes. All signals, so it is
/// `Copy` and a handler or a task takes it by value.
#[derive(Clone, Copy)]
struct Live {
    id: Signal<String>,
    tree: Signal<Option<RenderTree>>,
    cursor: Signal<Cursor>,
    focused: Signal<Option<String>>,
    /// The row whose menu is open, and the row being renamed: the mapping
    /// draws them, the page opens them (`m`, `r`) and the mapping closes them.
    menu: Signal<Option<RowRef>>,
    renaming: Signal<Option<RowRef>>,
    /// The row `x` cut, waiting for `p` or `P`.
    clipboard: Signal<Option<RowRef>>,
    session: Session,
    /// Whether `h` and `l` mean a layout verb here. A surface that fills a
    /// page says yes; one inside a layout pane says no, and the key bubbles to
    /// the layout page, which posts it once.
    layout_keys: bool,
}

impl Live {
    /// Replace the page's tree. The cursor follows the focused widget by id,
    /// so a row inserted above it does not move the focus to its neighbour.
    fn apply_tree(mut self, new: RenderTree) {
        if let Some(id) = self.focused.peek().clone() {
            self.cursor.write().set_focus(&new, &id);
        }
        let id = self.cursor.peek().focused_id(&new).map(str::to_string);
        if *self.focused.peek() != id {
            self.focused.set(id);
        }
        if self.tree.peek().as_ref() != Some(&new) {
            self.tree.set(Some(new));
        }
    }

    /// Render again and show it.
    async fn refresh(self) -> Result<(), ApiError> {
        let id = self.id.peek().clone();
        let tree = api::render(&id).await?;
        self.apply_tree(tree);
        self.session.clear();
        Ok(())
    }

    /// Show where the cursor is, and move DOM focus there.
    fn show_focus(mut self) {
        let id = {
            let tree = self.tree.peek();
            tree.as_ref()
                .and_then(|tree| self.cursor.peek().focused_id(tree).map(str::to_string))
        };
        if *self.focused.peek() != id {
            self.focused.set(id.clone());
        }
        if let Some(id) = id {
            spawn(async move {
                dom::focus_widget(&id).await;
                dom::scroll_active_row(&id).await;
            });
        }
    }

    /// A widget event from the surface (a click, a change): focus follows it,
    /// then it is posted.
    fn on_event(mut self, event: Event) {
        {
            let tree = self.tree.peek();
            if let Some(tree) = tree.as_ref() {
                let mut cursor = self.cursor.write();
                cursor.set_focus(tree, &event.widget);
                // A click on a row makes it the widget's active row.
                if let Some(row) = keys::selected_row(tree, &event) {
                    cursor.set_active_row(&event.widget, row);
                }
            }
        }
        let widget = event.widget.clone();
        if self.focused.peek().as_deref() != Some(widget.as_str()) {
            self.focused.set(Some(widget));
        }
        self.send(event);
    }

    /// A file chosen in a `file` field: store its bytes beside this surface
    /// (`PUT /api/blob/<surface id>/<name>`), then send the field's `change`
    /// carrying the blob reference. A refusal at the upload is the status
    /// line's, and no `change` is sent.
    fn on_file(self, chosen: FileChosen) {
        let id = self.id.peek().clone();
        let token = self.session.token.peek().clone();
        spawn(async move {
            match api::put_blob(&id, &chosen.name, chosen.bytes, token.as_ref()).await {
                Ok(()) => self.on_event(api::file_change_event(&chosen.widget, &id, &chosen.name)),
                Err(error) => self.session.refuse(error),
            }
        });
    }

    fn send(self, event: Event) {
        let id = self.id.peek().clone();
        let token = self.session.token.peek().clone();
        spawn(async move {
            match api::dispatch(&id, &event, token.as_ref()).await {
                Ok(result) => {
                    if let Some(tree) = result.tree.clone() {
                        self.apply_tree(tree);
                    }
                    match result.refusal() {
                        Some(refusal) => self.session.refuse(refusal),
                        None => self.session.clear(),
                    }
                }
                Err(error) => self.session.refuse(error),
            }
        });
    }

    fn on_keydown(mut self, event: dioxus::events::KeyboardEvent) {
        if self.menu.peek().is_some() && self.on_menu_key(&event) {
            return;
        }
        let action = {
            let tree = self.tree.peek();
            let Some(tree) = tree.as_ref() else { return };
            let mut cursor = self.cursor.write();
            keys::on_keydown(&event, tree, &mut cursor, self.layout_keys)
        };
        match action {
            KeyAction::Ignored => {}
            KeyAction::ReadBack => self.read_back(),
            KeyAction::Focus => self.show_focus(),
            KeyAction::Event(event) => {
                self.show_focus();
                self.send(event);
            }
            KeyAction::Layout(verb) => {
                let token = self.session.token.peek().clone();
                spawn(async move {
                    match api::layout_verb(&verb, token.as_ref()).await {
                        Ok(_) => self.session.clear(),
                        Err(error) => self.session.refuse(error),
                    }
                });
            }
            KeyAction::Rename(widget, row) => self.renaming.set(Some((widget, row))),
            KeyAction::Menu(widget, row) => self.menu.set(Some((widget, row))),
            KeyAction::Cut(widget, row) => self.clipboard.set(Some((widget, row))),
            paste @ (KeyAction::Paste(..) | KeyAction::PasteBeside(..)) => {
                let cut = self.clipboard.peek().clone();
                let event = {
                    let tree = self.tree.peek();
                    tree.as_ref()
                        .zip(cut)
                        .and_then(|(tree, cut)| keys::paste_event(tree, &cut, &paste))
                };
                if let Some(event) = event {
                    self.clipboard.set(None);
                    self.on_event(event);
                }
            }
        }
    }

    /// A key while a row menu is open: Escape closes it, a digit chooses that
    /// entry, Tab closes it and goes on as a Tab, and any other key is
    /// swallowed so the row cursor holds still under the menu. Returns whether
    /// the key was dealt with.
    fn on_menu_key(mut self, event: &dioxus::events::KeyboardEvent) -> bool {
        let Some((widget, row)) = self.menu.peek().clone() else {
            return false;
        };
        let name = event.key().to_string();
        if name == "Tab" {
            self.menu.set(None);
            return false;
        }
        let menu = self
            .tree
            .peek()
            .as_ref()
            .map(|tree| keys::menu_of(tree, &widget))
            .unwrap_or_default();
        event.prevent_default();
        // A digit indexes the entries the menu shows (`menu_entries`), and
        // the pick means what it means for a click (`MenuChoice`).
        match outline::menu_key(&name, menu.entries().len()) {
            MenuKey::Close => self.menu.set(None),
            MenuKey::Pick(index) => {
                self.menu.set(None);
                match menu.choice(index) {
                    Some(MenuChoice::Rename) => self.renaming.set(Some((widget, row))),
                    Some(MenuChoice::Delete) => self.on_event(Event {
                        widget,
                        kind: EventKind::Delete,
                        value: serde_json::Value::Array(vec![row]),
                    }),
                    Some(MenuChoice::Action(action)) => self.on_event(Event {
                        widget,
                        kind: EventKind::Menu,
                        value: outline::menu_value(&row, &action),
                    }),
                    None => {}
                }
            }
            MenuKey::Swallow => {}
        }
        true
    }

    /// After the browser's own Tab: the cursor goes where focus went.
    fn read_back(mut self) {
        spawn(async move {
            let Some(id) = dom::focused_widget().await else {
                return;
            };
            let known = {
                let tree = self.tree.peek();
                match tree.as_ref() {
                    Some(tree) => self.cursor.write().set_focus(tree, &id),
                    None => false,
                }
            };
            if known && self.focused.peek().as_deref() != Some(id.as_str()) {
                self.focused.set(Some(id));
            }
        });
    }
}

/// One surface, live. `embedded` is a surface inside a layout pane: no
/// breadcrumb, and `h`/`l` are left to the layout page around it.
#[component]
pub fn SurfaceView(id: String, #[props(default)] embedded: bool) -> Element {
    let session = use_context::<Session>();
    let live = Live {
        id: use_signal(|| id.clone()),
        tree: use_signal(|| None),
        cursor: use_signal(Cursor::new),
        focused: use_signal(|| None),
        menu: use_signal(|| None),
        renaming: use_signal(|| None),
        clipboard: use_signal(|| None),
        session,
        layout_keys: !embedded,
    };
    // The wait loop's flag. A task owned by this scope is dropped with it;
    // the flag is the explicit stop, checked before every write. A `Cell`,
    // not a `Signal<bool>`: a signal's storage is released with the scope, and
    // a drop hook that writes one can run after it.
    let alive = use_hook(|| Rc::new(Cell::new(true)));
    use_drop({
        let alive = alive.clone();
        move || alive.set(false)
    });

    use_future({
        let alive = alive.clone();
        move || {
            let alive = alive.clone();
            async move {
                let id = live.id.peek().clone();
                if let Err(error) = live.refresh().await {
                    session.refuse(error);
                }
                let mut after_seq = 0;
                while alive.get() {
                    match api::wait(&id, after_seq, WAIT_TIMEOUT_MS).await {
                        Ok(answer) => {
                            if !alive.get() {
                                break;
                            }
                            after_seq = answer.next_seq;
                            // An event, or a quiet window: either way, look.
                            if let Err(error) = live.refresh().await {
                                session.refuse(error);
                            }
                        }
                        Err(error) => {
                            session.refuse(error);
                            dom::sleep(RETRY_MS).await;
                        }
                    }
                }
            }
        }
    });

    let current = use_memo(move || live.tree.read().clone().unwrap_or_else(keys::empty_tree));
    let active = use_memo(move || keys::active_rows(&current.read(), &live.cursor.read()));

    // A right-click opens a menu in the mapping; the row it opened on becomes
    // the active row, so `m`, `r` and the click agree about which row it is.
    use_effect(move || {
        if let Some((widget, row)) = live.menu.read().clone() {
            let mut cursor = live.cursor;
            cursor.write().set_active_row(&widget, row);
        }
    });
    // A menu or an input that closes takes the DOM focus with it (the element
    // is gone): give it back to the widget so the next key lands.
    let mut overlay_open = use_signal(|| false);
    use_effect(move || {
        let open = live.menu.read().is_some() || live.renaming.read().is_some();
        if *overlay_open.peek() && !open {
            live.show_focus();
        }
        if *overlay_open.peek() != open {
            overlay_open.set(open);
        }
    });
    let sheet = use_memo(move || session.style.read().sheet().clone());
    let loaded = live.tree.read().is_some();
    // The surface's name, for the title: the list carries it and the render
    // does not. One fetch; a renamed surface shows its new name on the next visit.
    let title_id = id.clone();
    let name = use_resource(move || {
        let id = title_id.clone();
        async move {
            api::list()
                .await
                .ok()
                .and_then(|list| list.surfaces.into_iter().find(|s| s.id == id))
                .map(|s| s.title().to_string())
                .filter(|t| !t.is_empty())
        }
    });
    let name = move || name.read().clone().flatten();
    rsx! {
        if !embedded {
            p { class: "kw-note kw-crumb",
                Link { to: Route::Home {}, "surfaces" }
            }
            if let Some(name) = name() {
                h1 { "{name}" }
            }
        }
        div {
            class: "kw-surface",
            tabindex: "0",
            onkeydown: move |event| live.on_keydown(event),
            if loaded {
                Surface {
                    tree: current,
                    sheet,
                    focused: live.focused,
                    on_event: move |event| live.on_event(event),
                    on_file: move |chosen| live.on_file(chosen),
                    active,
                    menu: live.menu,
                    renaming: live.renaming,
                }
            } else {
                p { class: "kw-note", "rendering…" }
            }
        }
    }
}
