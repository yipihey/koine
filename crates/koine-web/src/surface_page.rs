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
use impress_surface::{Event, RenderKind, RenderNode, RenderTree};
use koine::cursor::Cursor;
use koine_dioxus::Surface;

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

/// The tree before the first render answers: nothing to draw.
fn empty_tree() -> RenderTree {
    RenderTree {
        root: RenderNode {
            id: String::new(),
            label: None,
            help: None,
            node: RenderKind::Column { items: Vec::new() },
        },
        focus_order: Vec::new(),
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
    session: Session,
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
            spawn(async move { dom::focus_widget(&id).await });
        }
    }

    /// A widget event from the surface (a click, a change): focus follows it,
    /// then it is posted.
    fn on_event(mut self, event: Event) {
        {
            let tree = self.tree.peek();
            if let Some(tree) = tree.as_ref() {
                self.cursor.write().set_focus(tree, &event.widget);
            }
        }
        let widget = event.widget.clone();
        if self.focused.peek().as_deref() != Some(widget.as_str()) {
            self.focused.set(Some(widget));
        }
        self.send(event);
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
        let action = {
            let tree = self.tree.peek();
            let Some(tree) = tree.as_ref() else { return };
            let mut cursor = self.cursor.write();
            keys::on_keydown(&event, tree, &mut cursor, true)
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
        }
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

#[component]
fn SurfaceView(id: String) -> Element {
    let session = use_context::<Session>();
    let live = Live {
        id: use_signal(|| id.clone()),
        tree: use_signal(|| None),
        cursor: use_signal(Cursor::new),
        focused: use_signal(|| None),
        session,
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

    let current = use_memo(move || live.tree.read().clone().unwrap_or_else(empty_tree));
    let sheet = use_memo(move || session.style.read().sheet().clone());
    let loaded = live.tree.read().is_some();
    rsx! {
        p { class: "kw-note",
            Link { to: Route::Home {}, "surfaces" }
            " / {id}"
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
                }
            } else {
                p { class: "kw-note", "rendering…" }
            }
        }
    }
}
