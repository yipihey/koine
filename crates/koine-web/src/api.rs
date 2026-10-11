//! The page's side of the app's HTTP surface routes.
//!
//! Everything that can be decided without a network is a pure function over a
//! `serde_json::Value` or a string and is tested natively: the URLs, the
//! typed results, and what a status code plus a body mean. The fetchers at the
//! bottom are the only code that touches `reqwest`, which is the browser's
//! `fetch` on wasm32.
//!
//! The wire shapes are `impress-surface-service`'s DTOs
//! (`SurfaceRenderResult`, `SurfaceDispatchResult`, `SurfaceWaitResult`,
//! `SurfaceListResult`) and are described in impress-apps
//! `docs/agent-surfaces.md` § HTTP. Only the fields the page reads are
//! mirrored; the rest are ignored, so a field the app adds does not break the
//! page.
//!
//! A refusal is [`ApiError`]: `{code, message}`, the words the app answered
//! with. The page draws it in the status line and invents no other error UI.

use impress_layout::Verb;
use impress_surface::{Event, RenderTree};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::token::Token;

/// The long-poll window the page asks the app for (D6).
pub const WAIT_TIMEOUT_MS: u64 = 25_000;

/// A refusal: the app's `code` and `message`, or the page's own for a
/// transport failure (`transport`), a body that is not the expected shape
/// (`bad-response`) and a 401 (`unauthorized`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// What every result carries: `ok`, a message, and a code when `ok` is false.
fn refusal_of(ok: bool, code: &Option<String>, message: &str) -> Option<ApiError> {
    (!ok).then(|| ApiError::new(code.as_deref().unwrap_or("refused"), message))
}

/// `GET /api/surface/<id>/render`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RenderResult {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub tree: Option<RenderTree>,
    /// Sources that failed on this render. The tree is still returned; the
    /// placeholders it draws already carry the reason.
    #[serde(default)]
    pub source_errors: Vec<Value>,
}

impl RenderResult {
    pub fn refusal(&self) -> Option<ApiError> {
        refusal_of(self.ok, &self.code, &self.message)
    }
}

/// `POST /api/surface/<id>/dispatch`. A dispatch whose effect failed is
/// `ok: false` and still carries the re-rendered `tree`: the state change
/// stands, so the page draws the tree and the refusal both.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DispatchResult {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub tree: Option<RenderTree>,
    #[serde(default)]
    pub effects: Vec<Value>,
    #[serde(default)]
    pub effects_failed: u32,
}

impl DispatchResult {
    pub fn refusal(&self) -> Option<ApiError> {
        refusal_of(self.ok, &self.code, &self.message)
    }
}

/// One row of `surface_events` / `surface_wait`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SurfaceEvent {
    pub seq: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub actor: String,
}

/// `GET /api/surface/<id>/wait`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WaitResult {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub events: Vec<SurfaceEvent>,
    #[serde(default)]
    pub next_seq: u64,
    #[serde(default)]
    pub timed_out: bool,
    /// The app dropped events older than `after_seq`; the page re-renders
    /// either way, so a gap needs no special handling.
    #[serde(default)]
    pub gap: bool,
}

impl WaitResult {
    pub fn refusal(&self) -> Option<ApiError> {
        refusal_of(self.ok, &self.code, &self.message)
    }
}

/// One surface in `GET /api/surface`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SurfaceSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `GET /api/surface`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SurfaceListResult {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub surfaces: Vec<SurfaceSummary>,
}

impl SurfaceListResult {
    pub fn refusal(&self) -> Option<ApiError> {
        refusal_of(self.ok, &self.code, &self.message)
    }
}

/// `POST /api/layout/verb`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LayoutVerbResult {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
}

impl LayoutVerbResult {
    pub fn refusal(&self) -> Option<ApiError> {
        refusal_of(self.ok, &self.code, &self.message)
    }
}

fn parse<T: for<'de> Deserialize<'de>>(value: &Value, what: &str) -> Result<T, ApiError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ApiError::new("bad-response", format!("{what}: {error}")))
}

pub fn parse_render(value: &Value) -> Result<RenderResult, ApiError> {
    parse(value, "render")
}

pub fn parse_dispatch(value: &Value) -> Result<DispatchResult, ApiError> {
    parse(value, "dispatch")
}

pub fn parse_wait(value: &Value) -> Result<WaitResult, ApiError> {
    parse(value, "wait")
}

pub fn parse_list(value: &Value) -> Result<SurfaceListResult, ApiError> {
    parse(value, "surface list")
}

pub fn parse_layout_verb(value: &Value) -> Result<LayoutVerbResult, ApiError> {
    parse(value, "layout verb")
}

/// What an HTTP answer means: the JSON body, or the refusal.
///
/// A 401 is `unauthorized` whatever the body says (the body of a 401 is the
/// server's, not a result). Any other status is read as its body: the app
/// answers a refusal as the same JSON shape with a status its code maps to,
/// so the `ok` and `code` inside are the truth. A body that is not JSON is
/// `bad-response`, carrying the status.
pub fn interpret(status: u16, body: &str) -> Result<Value, ApiError> {
    if status == 401 {
        return Err(ApiError::new(
            "unauthorized",
            "the app refused the request: no loopback token, or a stale one (the token changes at every launch)",
        ));
    }
    serde_json::from_str::<Value>(body).map_err(|_| {
        let snippet: String = body.chars().take(120).collect();
        ApiError::new("bad-response", format!("HTTP {status}: {snippet}"))
    })
}

/// Percent-encode one path segment or query value.
pub fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub fn list_url(origin: &str) -> String {
    format!("{}/api/surface", origin.trim_end_matches('/'))
}

pub fn render_url(origin: &str, id: &str) -> String {
    format!("{}/{}/render", list_url(origin), encode(id))
}

pub fn dispatch_url(origin: &str, id: &str) -> String {
    format!("{}/{}/dispatch", list_url(origin), encode(id))
}

pub fn wait_url(origin: &str, id: &str, after_seq: u64, timeout_ms: u64) -> String {
    format!(
        "{}/{}/wait?after_seq={after_seq}&timeout_ms={timeout_ms}",
        list_url(origin),
        encode(id)
    )
}

pub fn layout_verb_url(origin: &str) -> String {
    format!("{}/api/layout/verb", origin.trim_end_matches('/'))
}

/// The dispatch body: `{"event": …}`.
pub fn dispatch_body(event: &Event) -> Value {
    json!({ "event": event })
}

/// The layout verb body: the serialised verb, verbatim. The app forwards it
/// as it is to the layout (chassis matrix § HTTP automation).
pub fn layout_verb_body(verb: &Verb) -> Result<Value, ApiError> {
    serde_json::to_value(verb)
        .map_err(|error| ApiError::new("bad-request", format!("layout verb: {error}")))
}

/// The origin the page is served from, and so the origin of every request.
/// A browser build asks the window; a native build (desktop, tests) dials
/// impress's own port.
pub fn origin() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(origin) = web_sys::window().and_then(|window| window.location().origin().ok()) {
            return origin;
        }
    }
    "http://localhost:23125".to_string()
}

async fn get(url: &str) -> Result<Value, ApiError> {
    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(transport)?;
    answer(response).await
}

async fn post(url: &str, body: &Value, token: Option<&Token>) -> Result<Value, ApiError> {
    let mut request = reqwest::Client::new().post(url).json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token.as_str());
    }
    answer(request.send().await.map_err(transport)?).await
}

async fn answer(response: reqwest::Response) -> Result<Value, ApiError> {
    let status = response.status().as_u16();
    let body = response.text().await.map_err(transport)?;
    interpret(status, &body)
}

fn transport(error: reqwest::Error) -> ApiError {
    ApiError::new("transport", error.to_string())
}

/// The app's surfaces. A refusal is the `Err`.
pub async fn list() -> Result<SurfaceListResult, ApiError> {
    let result = parse_list(&get(&list_url(&origin())).await?)?;
    match result.refusal() {
        Some(refusal) => Err(refusal),
        None => Ok(result),
    }
}

/// One surface's tree. A refusal, or an answer with no tree, is the `Err`.
pub async fn render(id: &str) -> Result<RenderTree, ApiError> {
    let result = parse_render(&get(&render_url(&origin(), id)).await?)?;
    if let Some(refusal) = result.refusal() {
        return Err(refusal);
    }
    result
        .tree
        .ok_or_else(|| ApiError::new("bad-response", "render answered ok with no tree"))
}

/// Send one event. The `Ok` is the app's answer, which may itself be a
/// refusal that still carries a tree (`result.refusal()`); the `Err` is
/// everything else: the transport, a 401, a body that is not a result, a
/// refusal with no tree to draw.
pub async fn dispatch(
    id: &str,
    event: &Event,
    token: Option<&Token>,
) -> Result<DispatchResult, ApiError> {
    let body = post(&dispatch_url(&origin(), id), &dispatch_body(event), token).await?;
    let result = parse_dispatch(&body)?;
    match (result.refusal(), result.tree.is_some()) {
        (Some(refusal), false) => Err(refusal),
        _ => Ok(result),
    }
}

/// Long-poll for the next change. `timed_out` is not an error.
pub async fn wait(id: &str, after_seq: u64, timeout_ms: u64) -> Result<WaitResult, ApiError> {
    let result = parse_wait(&get(&wait_url(&origin(), id, after_seq, timeout_ms)).await?)?;
    match result.refusal() {
        Some(refusal) => Err(refusal),
        None => Ok(result),
    }
}

/// Send one layout verb. Needs the token.
pub async fn layout_verb(verb: &Verb, token: Option<&Token>) -> Result<LayoutVerbResult, ApiError> {
    let body = post(&layout_verb_url(&origin()), &layout_verb_body(verb)?, token).await?;
    let result = parse_layout_verb(&body)?;
    match result.refusal() {
        Some(refusal) => Err(refusal),
        None => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impress_surface::EventKind;

    fn tree_json() -> Value {
        json!({
            "root": {"id": "go", "node": {"kind": "button", "label": "Go"}},
            "focus_order": ["go"]
        })
    }

    #[test]
    fn a_render_answer_carries_the_tree_and_ignores_fields_it_does_not_read() {
        let body = json!({
            "ok": true, "message": "rendered", "tree": tree_json(),
            "revision": 3, "state_revision": 9, "wire_version": 1, "params": {}
        });
        let result = parse_render(&body).unwrap();
        assert!(result.refusal().is_none());
        assert_eq!(result.tree.unwrap().focus_order, vec!["go".to_string()]);
    }

    #[test]
    fn a_render_refusal_is_the_apps_code_and_message() {
        let body = json!({"ok": false, "code": "not-found", "message": "no surface 7"});
        let result = parse_render(&body).unwrap();
        assert_eq!(
            result.refusal(),
            Some(ApiError::new("not-found", "no surface 7"))
        );
        assert!(result.tree.is_none());
    }

    #[test]
    fn a_refusal_without_a_code_is_still_a_refusal() {
        let result = parse_render(&json!({"ok": false, "message": "no"})).unwrap();
        assert_eq!(result.refusal(), Some(ApiError::new("refused", "no")));
    }

    #[test]
    fn a_failed_effect_is_a_refusal_that_keeps_the_tree() {
        let body = json!({
            "ok": false, "code": "effect-failed",
            "message": "dispatched; 2 effect(s), 1 failed",
            "tree": tree_json(),
            "effects": [{"kind": "emit", "ok": true}, {"kind": "publish", "ok": false, "code": "no-pane"}],
            "effects_failed": 1
        });
        let result = parse_dispatch(&body).unwrap();
        assert_eq!(result.refusal().unwrap().code, "effect-failed");
        assert!(result.tree.is_some());
        assert_eq!(result.effects.len(), 2);
        assert_eq!(result.effects_failed, 1);
    }

    #[test]
    fn a_wait_answer_has_the_cursor_to_continue_from() {
        let body = json!({
            "ok": true, "message": "2 events",
            "events": [
                {"surface": "s", "host": "h", "seq": 4, "name": "went", "payload": {}, "at": "t", "actor": "agent"},
                {"seq": 5, "name": "again"}
            ],
            "next_seq": 5, "timed_out": false, "gap": false, "wire_version": 1
        });
        let result = parse_wait(&body).unwrap();
        assert_eq!(result.next_seq, 5);
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0].actor, "agent");
        assert!(!result.timed_out);

        let quiet = parse_wait(&json!({"ok": true, "next_seq": 5, "timed_out": true})).unwrap();
        assert!(quiet.timed_out && quiet.events.is_empty());
    }

    #[test]
    fn the_surface_list_names_each_surface() {
        let body = json!({
            "ok": true, "message": "2 surfaces",
            "surfaces": [
                {"id": "a1", "name": "Triage", "revision": 2, "tags": ["imbib"], "created": "t", "modified": "t"},
                {"id": "b2", "name": "Style"}
            ]
        });
        let result = parse_list(&body).unwrap();
        assert_eq!(result.surfaces.len(), 2);
        assert_eq!(result.surfaces[0].tags, vec!["imbib".to_string()]);
        assert_eq!(result.surfaces[1].name, "Style");
    }

    #[test]
    fn a_body_of_the_wrong_shape_is_bad_response() {
        let error = parse_wait(&json!({"message": "no ok field"})).unwrap_err();
        assert_eq!(error.code, "bad-response");
    }

    #[test]
    fn a_401_is_unauthorized_whatever_the_body_says() {
        let error = interpret(401, r#"{"ok": true}"#).unwrap_err();
        assert_eq!(error.code, "unauthorized");
        let error = interpret(401, "").unwrap_err();
        assert_eq!(error.code, "unauthorized");
    }

    #[test]
    fn a_refusal_status_is_read_as_its_json_body() {
        let value =
            interpret(404, r#"{"ok": false, "code": "not-found", "message": "x"}"#).unwrap();
        assert_eq!(
            parse_render(&value).unwrap().refusal().unwrap().code,
            "not-found"
        );
        let value = interpret(422, r#"{"ok": false, "code": "effect-failed", "message": "x", "tree": {"root": {"id": "go", "node": {"kind": "button", "label": "Go"}}, "focus_order": ["go"]}}"#).unwrap();
        assert!(parse_dispatch(&value).unwrap().tree.is_some());
    }

    #[test]
    fn a_body_that_is_not_json_is_bad_response_naming_the_status() {
        let error = interpret(502, "<html>Bad gateway</html>").unwrap_err();
        assert_eq!(error.code, "bad-response");
        assert!(error.message.starts_with("HTTP 502"), "{}", error.message);
    }

    #[test]
    fn urls_are_built_from_the_origin_and_the_encoded_id() {
        let origin = "http://localhost:23125/";
        assert_eq!(list_url(origin), "http://localhost:23125/api/surface");
        assert_eq!(
            render_url(origin, "7f3a-1"),
            "http://localhost:23125/api/surface/7f3a-1/render"
        );
        assert_eq!(
            dispatch_url(origin, "a b/c"),
            "http://localhost:23125/api/surface/a%20b%2Fc/dispatch"
        );
        assert_eq!(
            wait_url(origin, "x", 12, 25_000),
            "http://localhost:23125/api/surface/x/wait?after_seq=12&timeout_ms=25000"
        );
        assert_eq!(
            layout_verb_url(origin),
            "http://localhost:23125/api/layout/verb"
        );
    }

    #[test]
    fn the_dispatch_body_wraps_the_event_and_the_verb_body_is_the_verb() {
        let event = Event {
            widget: "go".into(),
            kind: EventKind::Click,
            value: Value::Null,
        };
        assert_eq!(
            dispatch_body(&event),
            json!({"event": {"widget": "go", "kind": "click", "value": null}})
        );
        let verb = Verb::FocusDirection {
            direction: impress_layout::Direction::Left,
        };
        assert_eq!(
            layout_verb_body(&verb).unwrap(),
            json!({"verb": "focus-direction", "direction": "left"})
        );
    }

    #[test]
    fn the_layout_verb_answer_is_ok_or_a_refusal() {
        assert!(parse_layout_verb(&json!({"ok": true, "wire_version": 1}))
            .unwrap()
            .refusal()
            .is_none());
        let refused = parse_layout_verb(
            &json!({"ok": false, "code": "conflict", "message": "stale revision"}),
        )
        .unwrap();
        assert_eq!(refused.refusal().unwrap().code, "conflict");
    }
}
