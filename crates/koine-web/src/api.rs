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

use impress_layout::{Layout, Verb};
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
    /// Why the dispatch did not land: the first failed effect's own code and
    /// message when one failed (the reason, say `empty-title: a note needs a
    /// title`, rather than "1 effect failed"), else the dispatch's refusal.
    pub fn refusal(&self) -> Option<ApiError> {
        let failed = self
            .effects
            .iter()
            .find(|e| e.get("ok") == Some(&Value::Bool(false)))
            .map(|e| {
                let text = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
                ApiError::new(
                    text("code").as_deref().unwrap_or("refused"),
                    text("message").unwrap_or_default(),
                )
            });
        failed.or_else(|| refusal_of(self.ok, &self.code, &self.message))
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
    /// A line about the surface, when the list carries one. The spec has no
    /// such field today; a list that grows one is shown under the title.
    #[serde(default)]
    pub description: Option<String>,
}

impl SurfaceSummary {
    /// What the home list calls the surface: its name (the spec's `name`),
    /// or its id when the name is blank.
    pub fn title(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// The description, when there is one that says something.
    pub fn description(&self) -> Option<&str> {
        self.description
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
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

/// `GET /api/layout/tree`: the window tree a host snapshots.
///
/// `layout` is `impress_layout::Layout`, deserialized as it is: the host writes
/// `Layout`'s own serde form, and `Layout` reads it through its `LayoutWire`
/// (so a tree written before `next_window` existed still loads). `revision` and
/// `app` are the host's and are optional on the wire. `focused` is the focused
/// tile (a leaf).
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutTree {
    pub version: u64,
    pub revision: Option<u64>,
    pub focused: Option<u64>,
    pub layout: Layout,
    pub app: Option<String>,
}

/// The wire shape of [`LayoutTree`], with the two refusal shapes the route
/// has: a failed snapshot (`snapshot_error`, `snapshot_code`) and the layout
/// routes' own `{ok: false, code, message}`.
#[derive(Deserialize)]
struct LayoutTreeWire {
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    version: u64,
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    focused: Option<u64>,
    #[serde(default)]
    layout: Option<Layout>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    snapshot_error: Option<String>,
    #[serde(default)]
    snapshot_code: Option<String>,
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

pub fn parse_layout_tree(value: &Value) -> Result<LayoutTree, ApiError> {
    let wire: LayoutTreeWire = parse(value, "layout tree")?;
    if let Some(message) = wire.snapshot_error {
        return Err(ApiError::new(
            wire.snapshot_code.as_deref().unwrap_or("snapshot-error"),
            message,
        ));
    }
    if wire.ok == Some(false) {
        return Err(ApiError::new(
            wire.code.as_deref().unwrap_or("refused"),
            wire.message.unwrap_or_default(),
        ));
    }
    let layout = wire
        .layout
        .ok_or_else(|| ApiError::new("bad-response", "layout tree: no `layout` in the answer"))?;
    Ok(LayoutTree {
        version: wire.version,
        revision: wire.revision,
        focused: wire.focused,
        layout,
        app: wire.app,
    })
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

/// The verb the layout routes run: linked when the host serves the layout
/// (`koine_host::routes::layout`). The inventory spells the layout service's
/// tools `layout-service_*`.
pub const LAYOUT_VERB: &str = "layout-service_get-layout";

/// `GET /api/verb/<name>/surface`: the one read-only route that answers
/// whether a verb is linked (200) or not (404 `not-found`). A GET, so it
/// needs no token.
pub fn verb_probe_url(origin: &str, name: &str) -> String {
    format!(
        "{}/api/verb/{}/surface",
        origin.trim_end_matches('/'),
        encode(name)
    )
}

/// What the probe's answer says: `Ok(true)` linked, `Ok(false)` the host
/// answered that no such verb is linked, `Err` anything else (a refusal of
/// another kind, a body that is not an envelope), which says nothing either
/// way.
pub fn parse_verb_probe(value: &Value) -> Result<bool, ApiError> {
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(true),
        Some(false) if value.get("code").and_then(Value::as_str) == Some("not-found") => Ok(false),
        _ => Err(ApiError::new(
            value
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("bad-response"),
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("verb probe: not an envelope")
                .to_string(),
        )),
    }
}

pub fn layout_tree_url(origin: &str) -> String {
    format!("{}/api/layout/tree", origin.trim_end_matches('/'))
}

pub fn layout_verb_url(origin: &str) -> String {
    format!("{}/api/layout/verb", origin.trim_end_matches('/'))
}

/// `PUT /api/blob/<item>/<name>`: the bytes of a file chosen in a `file` field,
/// kept beside the surface's item (plan-koine-live L-D6).
pub fn blob_url(origin: &str, item: &str, name: &str) -> String {
    format!(
        "{}/api/blob/{}/{}",
        origin.trim_end_matches('/'),
        encode(item),
        encode(name)
    )
}

/// The `change` a `file` field sends once its bytes are stored: the value is
/// a blob reference, never the bytes (`FieldKind::File`).
pub fn file_change_event(widget: &str, item: &str, name: &str) -> Event {
    Event {
        widget: widget.to_string(),
        kind: impress_surface::EventKind::Change,
        value: json!({ "blob": { "item": item, "name": name } }),
    }
}

/// What a blob `PUT` answered: `ok`, or the app's `{code, message}` refusal.
pub fn parse_put_blob(body: &Value) -> Result<(), ApiError> {
    if body.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let code = body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("refused");
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the app refused the upload");
    Err(ApiError::new(code, message))
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

/// The origin of every request: the installed [`HostLink`](crate::HostLink)'s
/// when there is one (a native app that linked its host, M-D2), else the
/// window's (the page is served by the app), else impress's own port, which is
/// what a native test dials.
pub fn origin() -> String {
    let window = {
        #[cfg(target_arch = "wasm32")]
        {
            web_sys::window().and_then(|window| window.location().origin().ok())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            None
        }
    };
    origin_of(crate::host_link().as_ref(), window)
}

/// [`origin`]'s rule, pure: the link, else the window's origin, else the
/// fallback.
pub fn origin_of(link: Option<&crate::HostLink>, window: Option<String>) -> String {
    link.map(|link| link.origin.trim_end_matches('/').to_string())
        .or(window)
        .unwrap_or_else(|| FALLBACK_ORIGIN.to_string())
}

/// impress's loopback port (`SiblingApp.descriptors`).
const FALLBACK_ORIGIN: &str = "http://localhost:23125";

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

async fn put_bytes(url: &str, bytes: Vec<u8>, token: Option<&Token>) -> Result<Value, ApiError> {
    let mut request = reqwest::Client::new()
        .put(url)
        .header("content-type", "application/octet-stream")
        .body(bytes);
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
    // The whole chain: reqwest's top line is "error sending request for url
    // (…)", and the reason (connection refused, a timeout, a resolver) is
    // two sources down. A phone shows this line and nothing else.
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(inner) = source {
        message.push_str(": ");
        message.push_str(&inner.to_string());
        source = inner.source();
    }
    ApiError::new("transport", message)
}

/// `GET /api/status` on the page's origin: what the host says it is, or the
/// transport's whole error chain. The Host page runs it so a phone can say
/// which layer fails.
pub async fn ping() -> Result<Value, ApiError> {
    get(&format!("{}/api/status", origin())).await
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

/// Store the bytes of a chosen file beside the surface `item`, under `name`.
/// Needs the token. The page then sends [`file_change_event`].
pub async fn put_blob(
    item: &str,
    name: &str,
    bytes: Vec<u8>,
    token: Option<&Token>,
) -> Result<(), ApiError> {
    let body = put_bytes(&blob_url(&origin(), item, name), bytes, token).await?;
    parse_put_blob(&body)
}

/// Long-poll for the next change. `timed_out` is not an error.
pub async fn wait(id: &str, after_seq: u64, timeout_ms: u64) -> Result<WaitResult, ApiError> {
    let result = parse_wait(&get(&wait_url(&origin(), id, after_seq, timeout_ms)).await?)?;
    match result.refusal() {
        Some(refusal) => Err(refusal),
        None => Ok(result),
    }
}

/// Whether the host links verb `name` ([`parse_verb_probe`]).
pub async fn verb_linked(name: &str) -> Result<bool, ApiError> {
    parse_verb_probe(&get(&verb_probe_url(&origin(), name)).await?)
}

/// The app's window tree. A refusal, or a snapshot that failed, is the `Err`.
/// There is no wait route for the layout, so the layout page polls this.
pub async fn layout_tree() -> Result<LayoutTree, ApiError> {
    parse_layout_tree(&get(&layout_tree_url(&origin())).await?)
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

    fn link(origin: &str) -> crate::HostLink {
        crate::HostLink {
            origin: origin.into(),
            token: Some("t".into()),
            app_id: "demo".into(),
        }
    }

    #[test]
    fn a_verb_probe_says_linked_not_linked_or_nothing() {
        assert_eq!(
            verb_probe_url("http://h:1/", LAYOUT_VERB),
            "http://h:1/api/verb/layout-service_get-layout/surface"
        );
        assert_eq!(
            parse_verb_probe(&json!({"ok": true, "verb": LAYOUT_VERB, "spec": {}})),
            Ok(true)
        );
        assert_eq!(
            parse_verb_probe(&json!({"ok": false, "code": "not-found", "message": "no verb"})),
            Ok(false)
        );
        // Another refusal, or no envelope, says nothing about linking.
        assert_eq!(
            parse_verb_probe(&json!({"ok": false, "code": "internal", "message": "x"}))
                .unwrap_err()
                .code,
            "internal"
        );
        assert!(parse_verb_probe(&json!([1])).is_err());
    }

    #[test]
    fn the_origin_is_the_link_then_the_window_then_the_fallback() {
        let l = link("http://127.0.0.1:4000/");
        assert_eq!(
            origin_of(Some(&l), Some("http://win".into())),
            "http://127.0.0.1:4000"
        );
        assert_eq!(origin_of(None, Some("http://win".into())), "http://win");
        assert_eq!(origin_of(None, None), "http://localhost:23125");
    }

    #[test]
    fn an_installed_link_is_what_origin_and_host_link_answer() {
        use crate::{clear_link, host_link, install_link, LINK_TESTS};
        let _guard = LINK_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        clear_link();
        assert_eq!(host_link(), None);
        assert_eq!(origin(), "http://localhost:23125");
        let first = link("http://127.0.0.1:4001");
        install_link(first.clone());
        assert_eq!(host_link(), Some(first));
        assert_eq!(origin(), "http://127.0.0.1:4001");
        // A second install replaces the first.
        install_link(link("http://127.0.0.1:4002"));
        assert_eq!(origin(), "http://127.0.0.1:4002");
        clear_link();
        assert_eq!(origin(), "http://localhost:23125");
    }

    fn tree_json() -> Value {
        json!({
            "root": {"id": "go", "node": {"kind": "button", "label": "Go"}},
            "focus_order": ["go"]
        })
    }

    #[test]
    fn a_blob_url_encodes_the_item_and_the_name() {
        assert_eq!(
            blob_url("http://localhost:23125/", "s-1", "my data.csv"),
            "http://localhost:23125/api/blob/s-1/my%20data.csv"
        );
        assert_eq!(
            blob_url("http://h", "s", "a/b.csv"),
            "http://h/api/blob/s/a%2Fb.csv"
        );
    }

    #[test]
    fn a_stored_file_is_a_change_carrying_a_blob_reference() {
        let event = file_change_event("upload", "surface-1", "data.csv");
        assert_eq!(event.widget, "upload");
        assert_eq!(event.kind, EventKind::Change);
        assert_eq!(
            event.value,
            json!({"blob": {"item": "surface-1", "name": "data.csv"}})
        );
    }

    #[test]
    fn a_blob_put_answer_is_ok_or_the_apps_refusal() {
        assert!(parse_put_blob(&json!({
            "ok": true, "wire_version": 1, "item": "x", "name": "n", "bytes": 3
        }))
        .is_ok());
        let refused = parse_put_blob(&json!({
            "ok": false, "code": "not-found", "message": "the store does not hold blobs"
        }))
        .unwrap_err();
        assert_eq!(refused.code, "not-found");
        assert!(refused.message.contains("blobs"));
        assert_eq!(parse_put_blob(&json!({})).unwrap_err().code, "refused");
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
            "effects": [{"kind": "emit", "ok": true}, {"kind": "publish", "ok": false, "code": "no-pane", "message": "no pane shows this surface"}],
            "effects_failed": 1
        });
        let result = parse_dispatch(&body).unwrap();
        // The reason, not the count.
        assert_eq!(
            result.refusal(),
            Some(ApiError::new("no-pane", "no pane shows this surface"))
        );
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
        assert_eq!(result.surfaces[1].description, None);
    }

    #[test]
    fn a_surface_is_titled_by_its_name_or_else_its_id() {
        let body = json!({
            "ok": true,
            "surfaces": [
                {"id": "a1", "name": "Triage", "description": " a line "},
                {"id": "b2", "name": "   "},
                {"id": "c3", "description": ""}
            ]
        });
        let result = parse_list(&body).unwrap();
        let rows = &result.surfaces;
        assert_eq!(rows[0].title(), "Triage");
        assert_eq!(rows[0].description(), Some("a line"));
        assert_eq!(rows[1].title(), "b2");
        assert_eq!(rows[2].title(), "c3");
        assert_eq!(rows[2].description(), None);
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
        assert_eq!(
            layout_tree_url(origin),
            "http://localhost:23125/api/layout/tree"
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

    /// The layout `impress-layout`'s golden test pins: three panes in a
    /// horizontal split, shares 1:2:3.
    const THREE_COLUMN: &str = include_str!("../../impress-layout/tests/golden/three_column.json");

    #[test]
    fn a_layout_tree_answer_carries_the_layout_the_focus_and_the_revision() {
        let layout: Value = serde_json::from_str(THREE_COLUMN).unwrap();
        let body = json!({
            "version": 7, "revision": 12, "focused": 2, "app": "imbib",
            "layout": layout, "wire_version": 1
        });
        let tree = parse_layout_tree(&body).unwrap();
        assert_eq!(tree.version, 7);
        assert_eq!(tree.revision, Some(12));
        assert_eq!(tree.focused, Some(2));
        assert_eq!(tree.app.as_deref(), Some("imbib"));
        assert_eq!(tree.layout.windows.len(), 1);
        assert_eq!(tree.layout.tiles.len(), 4);
        assert_eq!(tree.layout.windows[0].root.raw(), 4);
    }

    #[test]
    fn the_hosts_minimal_answer_needs_no_revision_or_app() {
        let body = json!({
            "version": 1,
            "layout": {"windows": [{"id": 1, "root": 1, "focused": 1}], "tiles": {
                "1": {"pane": {"view_kind": "placeholder"}}
            }}
        });
        let tree = parse_layout_tree(&body).unwrap();
        assert_eq!(tree.revision, None);
        assert_eq!(tree.focused, None);
        assert_eq!(tree.layout.windows[0].id.raw(), 1);
    }

    #[test]
    fn a_failed_snapshot_is_the_apps_code_and_message() {
        let body = json!({
            "version": 7, "snapshot_error": "store: disk I/O error",
            "snapshot_code": "store-error"
        });
        assert_eq!(
            parse_layout_tree(&body).unwrap_err(),
            ApiError::new("store-error", "store: disk I/O error")
        );
        let uncoded = json!({"version": 7, "snapshot_error": "locked"});
        assert_eq!(
            parse_layout_tree(&uncoded).unwrap_err().code,
            "snapshot-error"
        );
    }

    #[test]
    fn a_route_refusal_and_a_body_with_no_layout_are_errors() {
        let refused = json!({"ok": false, "code": "unavailable", "message": "no window"});
        assert_eq!(
            parse_layout_tree(&refused).unwrap_err(),
            ApiError::new("unavailable", "no window")
        );
        let empty = parse_layout_tree(&json!({"version": 1})).unwrap_err();
        assert_eq!(empty.code, "bad-response");
        // A pane with no view kind is not a `PaneSpec`: say so, do not guess.
        let broken = json!({"version": 1, "layout": {"tiles": {"1": {"pane": {}}}}});
        assert_eq!(parse_layout_tree(&broken).unwrap_err().code, "bad-response");
    }
}
