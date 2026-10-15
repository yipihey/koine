//! `/ask`: ask an agent (docs/plan-koine-research-os.md G-D5).
//!
//! A text area, two buttons — ask a question, or ask for a view — and the
//! desk below them: every request with its status and answer, an answered
//! view one link away. The page files with `agent-service_ask` (or
//! `agent-service_make-surface`) and lists with `agent-service_requests`,
//! both through `POST /api/verb/<name>`; it decides nothing about what an
//! agent does. `?about=<surface id>` is the surface the person came from (the
//! `a` key on a surface page, `keys::KeyAction::Ask`); while "Include the
//! surface I was looking at" is on, the question carries it as context.
//!
//! Three paths, one action (plan-koine-affordances A-D2): the nav's "ask"
//! item for a mouse and a finger, `a` for the keyboard, and the same form.

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, ApiError};
use crate::session::Session;
use crate::{dom, Route};

/// The verbs the page calls.
pub const ASK_VERB: &str = "agent-service_ask";
pub const MAKE_VERB: &str = "agent-service_make-surface";
pub const REQUESTS_VERB: &str = "agent-service_requests";

/// How often the desk is read while the page is visible.
const POLL_MS: u32 = 3_000;

/// What the person asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asking {
    Question,
    View,
}

/// The verb and arguments for one submission: a question carries the surface
/// the person came from when `attach` is on; a view request carries the text
/// as its description. `None` for an empty prompt.
pub fn submission(
    asking: Asking,
    prompt: &str,
    about: &str,
    attach: bool,
) -> Option<(&'static str, Value)> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return None;
    }
    Some(match asking {
        Asking::Question => {
            let mut args = json!({ "prompt": prompt });
            if attach && !about.trim().is_empty() {
                args["surface_id"] = json!(about.trim());
            }
            (ASK_VERB, args)
        }
        Asking::View => (MAKE_VERB, json!({ "description": prompt })),
    })
}

/// One request on the desk, as the page lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeskRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub prompt: String,
    pub answer: Option<String>,
    pub surface_id: Option<String>,
    pub claimed_by: Option<String>,
}

/// The rows of an `agent-service_requests` answer.
pub fn desk_rows(answer: &Value) -> Vec<DeskRow> {
    let text = |row: &Value, field: &str| {
        row[field]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    answer["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| DeskRow {
                    id: text(row, "id").unwrap_or_default(),
                    kind: text(row, "kind").unwrap_or_default(),
                    status: text(row, "status").unwrap_or_default(),
                    prompt: text(row, "prompt").unwrap_or_default(),
                    answer: text(row, "answer"),
                    surface_id: text(row, "surface_id"),
                    claimed_by: text(row, "claimed_by"),
                })
                .collect()
        })
        .unwrap_or_default()
}

const ASK_CSS: &str = r#"
.kw-ask textarea { width: 100%; min-height: 96px; box-sizing: border-box; padding: 10px 12px;
  font: inherit; border-radius: 10px; border: var(--koine-hairline) solid var(--koine-separator);
  background: var(--koine-bg-field, transparent); color: inherit; resize: vertical; }
.kw-ask-actions { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin: 10px 0; }
.kw-ask-attach { display: flex; gap: 6px; align-items: center; font-size: 13px; }
.kw-desk { list-style: none; margin: 0; padding: 0; }
.kw-desk li { padding: 10px 16px; border-bottom: var(--koine-hairline) solid var(--koine-separator); }
.kw-desk-head { display: flex; gap: 8px; font-size: 12px; color: var(--koine-color-secondary); }
.kw-desk-status { font-weight: 600; text-transform: uppercase; }
.kw-desk-answer { margin: 6px 0 0; }
@media (max-width: 700px), (pointer: coarse) {
  .kw-ask textarea, .kw-ask button { font-size: max(1em, 16px); }
  .kw-ask-actions button { min-height: 44px; flex: 1 1 auto; }
}
"#;

#[component]
pub fn AskPage(about: String) -> Element {
    let session = use_context::<Session>();
    let mut prompt = use_signal(String::new);
    let mut attach = use_signal(|| true);
    let mut filed: Signal<Option<Value>> = use_signal(|| None);
    let mut desk: Signal<Option<Vec<DeskRow>>> = use_signal(|| None);
    let mut busy = use_signal(|| false);
    let about = use_signal(|| about.clone());
    let alive = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(true)));
    use_drop({
        let alive = alive.clone();
        move || alive.set(false)
    });
    // The desk, read on mount and every POLL_MS while the tab is visible.
    let read_desk = move || async move {
        let token = session.token.peek().clone();
        match api::call_verb(REQUESTS_VERB, &json!({}), token.as_ref()).await {
            Ok(answer) => {
                let rows = desk_rows(&answer);
                if desk.peek().as_ref() != Some(&rows) {
                    desk.set(Some(rows));
                }
                session.clear_read();
            }
            Err(error) => session.refuse_read(error),
        }
    };
    use_future(move || {
        let alive = alive.clone();
        async move {
            // The desk is read with a POST, which needs the token the root
            // loads at startup: wait for it (briefly) rather than earn a 401.
            for _ in 0..25 {
                if session.token.peek().is_some() || !alive.get() {
                    break;
                }
                dom::sleep(200).await;
            }
            while alive.get() {
                if !dom::page_hidden().await {
                    read_desk().await;
                }
                dom::sleep(POLL_MS).await;
            }
        }
    });
    let mut submit = move |asking: Asking| {
        let Some((verb, args)) = submission(asking, &prompt.peek(), &about.peek(), *attach.peek())
        else {
            session.refuse(ApiError::new(
                "invalid-argument",
                "write what you want to ask first",
            ));
            return;
        };
        busy.set(true);
        let token = session.token.peek().clone();
        spawn(async move {
            match api::call_verb(verb, &args, token.as_ref()).await {
                Ok(answer) => {
                    session.clear();
                    filed.set(Some(answer));
                    prompt.set(String::new());
                    read_desk().await;
                }
                Err(error) => session.refuse(error),
            }
            busy.set(false);
        });
    };
    let about_text = about.read().clone();
    rsx! {
        style { dangerous_inner_html: ASK_CSS }
        h1 { "Ask an agent" }
        div { class: "kw-ask",
            p { class: "kw-note",
                "Whichever agent is connected to this app picks this up from the desk and answers here."
            }
            textarea {
                id: "kw-ask-prompt",
                "data-koine-id": "ask-prompt",
                aria_label: "What to ask",
                placeholder: "Which of these notes are about the same thing? / A table of …",
                value: "{prompt}",
                autofocus: true,
                oninput: move |event| prompt.set(event.value()),
            }
            div { class: "kw-ask-actions",
                button {
                    class: "koine-button",
                    id: "kw-ask-question",
                    disabled: *busy.read(),
                    onclick: move |_| submit(Asking::Question),
                    "Ask"
                }
                button {
                    class: "koine-button",
                    id: "kw-ask-view",
                    disabled: *busy.read(),
                    onclick: move |_| submit(Asking::View),
                    "Ask for a view"
                }
                if !about_text.is_empty() {
                    label { class: "kw-ask-attach",
                        input {
                            r#type: "checkbox",
                            id: "kw-ask-attach",
                            checked: *attach.read(),
                            onchange: move |event| attach.set(event.checked()),
                        }
                        "Include the surface I was looking at"
                    }
                }
            }
            if let Some(answer) = &*filed.read() {
                p { class: "kw-note", id: "kw-ask-filed",
                    "Filed request {answer[\"request\"][\"id\"].as_str().unwrap_or(\"?\")}."
                }
            }
        }
        h2 { "Desk" }
        match &*desk.read() {
            None => rsx! { p { class: "kw-note", "reading the desk…" } },
            Some(rows) if rows.is_empty() => rsx! {
                p { class: "kw-note", "Nothing asked yet." }
            },
            Some(rows) => rsx! {
                ul { class: "kw-desk", id: "kw-desk",
                    for row in rows.iter() {
                        li { key: "{row.id}", "data-request": "{row.id}",
                            div { class: "kw-desk-head",
                                span { class: "kw-desk-status", "{row.status}" }
                                span { "{row.kind}" }
                                if let Some(agent) = &row.claimed_by {
                                    span { "{agent}" }
                                }
                            }
                            div { "{row.prompt}" }
                            if let Some(answer) = &row.answer {
                                p { class: "kw-desk-answer", "{answer}" }
                            }
                            if let Some(surface) = &row.surface_id {
                                Link { to: Route::SurfacePage { id: surface.clone() }, "Open the view" }
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
    use super::*;

    #[test]
    fn a_question_carries_the_surface_only_when_attached() {
        let (verb, args) = submission(Asking::Question, "  why? ", "s-1", true).unwrap();
        assert_eq!(verb, ASK_VERB);
        assert_eq!(args, json!({"prompt": "why?", "surface_id": "s-1"}));
        let (_, args) = submission(Asking::Question, "why?", "s-1", false).unwrap();
        assert_eq!(args, json!({"prompt": "why?"}));
        let (_, args) = submission(Asking::Question, "why?", "", true).unwrap();
        assert_eq!(args, json!({"prompt": "why?"}));
        let (verb, args) = submission(Asking::View, "a table", "s-1", true).unwrap();
        assert_eq!(verb, MAKE_VERB);
        assert_eq!(args, json!({"description": "a table"}));
        assert!(submission(Asking::Question, "   ", "", true).is_none());
    }

    #[test]
    fn the_desk_rows_are_read_off_the_requests_answer() {
        let rows = desk_rows(&json!({"ok": true, "rows": [
            {"id": "r1", "kind": "ask", "status": "answered", "prompt": "p",
             "answer": "a", "surface_id": "s", "claimed_by": "agent:claude"},
            {"id": "r2", "kind": "make-surface", "status": "open", "prompt": "q",
             "answer": null}
        ]}));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].surface_id.as_deref(), Some("s"));
        assert_eq!(rows[1].answer, None);
        assert!(desk_rows(&json!({"ok": true})).is_empty());
    }
}
