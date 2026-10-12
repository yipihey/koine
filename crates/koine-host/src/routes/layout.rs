//! `/api/layout/{tree,layouts,verb,op}`: the layout, as the verbs that make it.
//!
//! The Swift app answers these four from its window's live tree
//! (`LayoutAutomationRoutes`). A headless host has no window, so each is
//! answered from the stored `impress/ui/layout@1.0.0` row for
//! `(Config::app_id, Config::device)` by the same verbs an agent calls over
//! MCP, run through the pipeline as the HTTP agent:
//!
//! | Route | Runs | Where it differs from the app |
//! |---|---|---|
//! | `GET tree` | `layout-service_get-layout` | `version` is the row's `revision` (there is no controller counter); a cold start writes the default layout, attributed to the agent, as the verb documents |
//! | `GET layouts` | `layout-service_list-layouts` | none: the saved layouts in ⌃⌘1–9 order |
//! | `POST verb` | `layout-service_apply` (below) | the result is the verb envelope (`affected_panes`, `revision`, `focused`, `patch`), not the whole tree: `GET tree` is how a page re-reads it |
//! | `POST op` `undo` / `redo` | `layout-service_undo` / `_redo` | `stack` is required, `pane` names the exploration ring's pane |
//! | `POST op` `save-layout` | `layout-service_save-layout` | none |
//! | `POST op` `apply-layout` | `layout-service_apply-layout` | `name` or `ordinal` |
//! | `POST op` `delete-layout` | `layout-service_delete-layout` | `name` is `name_or_id` |
//! | `POST op` `resize-share` | `Resize` through `apply` | no verb gives one pane a share: the container's shares are computed from the stored tree and applied as one `Resize` (the Swift FFI's rule, with the same read-then-write gap) |
//!
//! **`layout-service_apply` is not an inventory verb.** The layout service has
//! one method per gesture and none that takes a raw `impress_layout::Verb`,
//! which is what `POST verb` carries. So the route runs
//! `DefaultLayoutService::apply_verb_as` inside the pipeline under a
//! descriptor of that name, exactly as the Swift FFI's `apply` does: the call
//! is policy-checked, spanned, audited and attributed to `Agent("http")` like
//! any verb, but it cannot be named by `POST /api/verb/…` or MCP.

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use impress_item::ActorKind;
use impress_layout::{Container, Layout, Tile, TileId, Verb, HIDDEN_SHARE};
use impress_layout_service::DefaultLayoutService;
use impress_service_core::pipeline::{self, Call, CallerIdentity, PipelineError};
use impress_service_core::refusal::{codes, Refusal};
use impress_service_core::wire::WIRE_VERSION;
use impress_service_core::{BoxError, Effects, Kind, Safety, SafetyClass, Source, VerbDescriptor};
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

use super::{call_verb, json_response, refusal_body, status_of, Shared};

const OPERATIONS: [&str; 6] = [
    "undo",
    "redo",
    "resize-share",
    "save-layout",
    "apply-layout",
    "delete-layout",
];

pub(super) async fn tree(State(state): State<Shared>) -> Response {
    let (status, answer) = get_layout(&state).await;
    if status != 200 {
        return json_response(status, &answer);
    }
    let revision = answer.get("revision").cloned().unwrap_or(Value::Null);
    json_response(
        200,
        &json!({
            "ok": true,
            "wire_version": WIRE_VERSION,
            "app": state.app_id,
            "device": answer.get("device").cloned().unwrap_or(json!(state.device)),
            // The app's counter is per controller; this host has the row's.
            "version": revision.as_u64().unwrap_or(0),
            "revision": revision,
            "focused": answer.get("focused").cloned().unwrap_or(Value::Null),
            "layout": answer.get("layout").cloned().unwrap_or(Value::Null),
            "item_id": answer.get("item_id").cloned().unwrap_or(Value::Null),
            "store": answer.get("store").cloned().unwrap_or(Value::Null),
        }),
    )
}

pub(super) async fn layouts(State(state): State<Shared>) -> Response {
    let (status, answer) = call_verb(
        "layout-service_list-layouts",
        json!({ "app_id": state.app_id }),
    )
    .await;
    if status != 200 {
        return json_response(status, &answer);
    }
    json_response(
        200,
        &json!({
            "ok": true,
            "wire_version": WIRE_VERSION,
            "app": state.app_id,
            "layouts": answer.get("layouts").cloned().unwrap_or(json!([])),
        }),
    )
}

pub(super) async fn verb(State(state): State<Shared>, body: Bytes) -> Response {
    let (verb, expected) = match parse_verb(&body) {
        Ok(parsed) => parsed,
        Err(refusal) => return refused(refusal),
    };
    let (status, answer) = apply(&state, verb, expected).await;
    json_response(status, &answer)
}

pub(super) async fn op(State(state): State<Shared>, body: Bytes) -> Response {
    let (status, answer) = run_op(&state, &body).await;
    json_response(status, &answer)
}

// ------------------------------------------------------------------- reads

async fn get_layout(state: &Shared) -> (u16, Value) {
    call_verb(
        "layout-service_get-layout",
        json!({ "app_id": state.app_id, "device": state.device }),
    )
    .await
}

// -------------------------------------------------------------------- ops

async fn run_op(state: &Shared, body: &[u8]) -> (u16, Value) {
    let object = match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(object)) => object,
        _ => {
            return bad(format!(
                "Expected {{\"op\": ...}} — one of {}",
                OPERATIONS.join(", ")
            ))
        }
    };
    let Some(op) = object.get("op").and_then(Value::as_str) else {
        return bad(format!(
            "Expected {{\"op\": ...}} — one of {}",
            OPERATIONS.join(", ")
        ));
    };
    let expected = match expected_revision(&object) {
        Ok(expected) => expected,
        Err(refusal) => return failed(refusal),
    };
    let app = &state.app_id;
    let device = &state.device;
    match op {
        "undo" | "redo" => {
            let Some(stack) = object.get("stack").and_then(Value::as_str) else {
                // The stack is explicit and has no default: an "undo" that
                // silently meant "the window's shape" would eventually undo an
                // arrangement an agent thought it was undoing a selection on.
                return bad(format!(
                    "{op} needs {{\"stack\": \"arrangement\"|\"exploration\"}}"
                ));
            };
            let mut args = json!({
                "app_id": app, "device": device, "stack": stack,
                "expected_revision": expected,
            });
            if let Some(pane) = object.get("pane").and_then(Value::as_u64) {
                args["target"] = json!({ "id": pane });
            }
            call_verb(&format!("layout-service_{op}"), args).await
        }
        "resize-share" => {
            let (Some(pane), Some(share)) = (
                object.get("pane").and_then(Value::as_u64),
                object.get("share").and_then(Value::as_f64),
            ) else {
                return bad("resize-share needs {\"pane\": <tile id>, \"share\": <0…1>}".into());
            };
            let (status, current) = get_layout(state).await;
            if status != 200 {
                return (status, current);
            }
            let layout = match current
                .get("layout")
                .cloned()
                .map(serde_json::from_value::<Layout>)
            {
                Some(Ok(layout)) => layout,
                _ => {
                    return failed(Refusal::internal(
                        "the stored layout did not parse as a layout",
                    ))
                }
            };
            match resize_one_share(&layout, TileId::new(pane), share as f32) {
                Ok(resize) => apply(state, resize, expected).await,
                Err(refusal) => failed(refusal),
            }
        }
        "save-layout" => {
            let Some(name) = non_empty(&object, "name") else {
                return bad("save-layout needs {\"name\": ...}".into());
            };
            call_verb(
                "layout-service_save-layout",
                json!({
                    "app_id": app, "device": device, "name": name,
                    "purpose": object.get("purpose").cloned().unwrap_or(Value::Null),
                }),
            )
            .await
        }
        "apply-layout" => {
            let name = non_empty(&object, "name");
            let ordinal = object.get("ordinal").and_then(Value::as_u64);
            if name.is_none() && ordinal.is_none() {
                return bad("apply-layout needs {\"name\": ...} or {\"ordinal\": 1…9}".into());
            }
            call_verb(
                "layout-service_apply-layout",
                json!({
                    "app_id": app, "device": device, "name": name, "ordinal": ordinal,
                    "expected_revision": expected,
                }),
            )
            .await
        }
        "delete-layout" => {
            let Some(name) = non_empty(&object, "name") else {
                return bad("delete-layout needs {\"name\": ...}".into());
            };
            call_verb(
                "layout-service_delete-layout",
                json!({ "app_id": app, "name_or_id": name }),
            )
            .await
        }
        other => bad(format!(
            "Unknown layout operation '{other}'. Known: {}. Tree-shaped verbs go to \
             POST /api/layout/verb.",
            OPERATIONS.join(", ")
        )),
    }
}

fn non_empty<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn expected_revision(object: &Map<String, Value>) -> Result<Option<u64>, Refusal> {
    match object.get("expected_revision") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            Refusal::invalid_argument(format!(
                "expected_revision must be a non-negative integer, not {value}"
            ))
        }),
    }
}

fn bad(message: String) -> (u16, Value) {
    failed(Refusal::invalid_argument(message))
}

fn failed(refusal: Refusal) -> (u16, Value) {
    (
        refusal.http_status(),
        refusal_body(&refusal.code, refusal.message),
    )
}

fn refused(refusal: Refusal) -> Response {
    let (status, body) = failed(refusal);
    json_response(status, &body)
}

// ------------------------------------------------------------------- apply

/// `Verb`'s JSON schema, for the strict check (generated once).
fn verb_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(impress_service_core::schemars::schema_for!(Verb))
            .unwrap_or(Value::Null)
    })
}

/// A verb as `POST /api/layout/verb` takes it: the verb's own fields,
/// strictly (a field the schema does not name is refused, naming it), plus an
/// optional `expected_revision`.
fn parse_verb(body: &[u8]) -> Result<(Verb, Option<u64>), Refusal> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        Refusal::invalid_argument(
            "Expected an impress_layout::Verb JSON object, e.g. \
             {\"verb\":\"focus\",\"target\":{\"role\":\"detail\"}} \
             — optionally with \"expected_revision\": N",
        )
    })?;
    let Value::Object(mut object) = value else {
        return Err(Refusal::invalid_argument(
            "a verb is a JSON object with a \"verb\" field",
        ));
    };
    let expected = expected_revision(&object)?;
    object.remove("expected_revision");
    let verb =
        impress_service_core::strict::args::<Verb>("verb", Value::Object(object), verb_schema())?;
    Ok((verb, expected))
}

/// Apply one verb to the stored live layout, through the pipeline.
async fn apply(state: &Shared, verb: Verb, expected: Option<u64>) -> (u16, Value) {
    let app = state.app_id.clone();
    let device = state.device.clone();
    let call = Call::new(
        CallerIdentity::agent("http"),
        json!({ "verb": verb, "expected_revision": expected }),
    );
    let answer = pipeline::invoke_with(&LAYOUT_APPLY, call, |_| async move {
        let result = DefaultLayoutService::new().apply_verb_as(
            &app,
            Some(device),
            ActorKind::Agent,
            expected,
            |_| Ok(verb),
        );
        serde_json::to_value(result).map_err(|e| Box::new(e) as BoxError)
    })
    .await;
    match answer {
        Ok(value) => (status_of(&value), value),
        Err(PipelineError::Handler(error)) => {
            (502, refusal_body(codes::VERB_FAILED, error.to_string()))
        }
        Err(unavailable) => (
            503,
            refusal_body(codes::HOST_UNAVAILABLE, unavailable.to_string()),
        ),
    }
}

/// The `Resize` that gives one pane `share` in its parent split and leaves
/// its siblings alone — computed from the tree the verb will apply to.
fn resize_one_share(layout: &Layout, tile: TileId, share: f32) -> Result<Verb, Refusal> {
    let parent = layout.parent_of(tile).ok_or_else(|| {
        Refusal::new("not-in-a-split", format!("tile {tile} has no parent split"))
    })?;
    let container = layout
        .tile(parent)
        .and_then(Tile::as_container)
        .ok_or_else(|| {
            Refusal::new(
                "not-a-container",
                format!("tile {parent} is not a container"),
            )
        })?;
    if !matches!(container, Container::Linear { .. }) {
        return Err(Refusal::new(
            "not-in-a-split",
            format!(
                "tile {parent} is a {:?}, and only a split has shares",
                container.kind()
            ),
        ));
    }
    let index = container.index_of(tile).ok_or_else(|| {
        Refusal::new(
            "unknown-tile",
            format!("tile {tile} is not a child of {parent}"),
        )
    })?;
    let existing = container.shares().unwrap_or(&[]);
    let mut shares: Vec<f32> = (0..container.len())
        .map(|i| existing.get(i).copied().unwrap_or(1.0))
        .collect();
    shares[index] = share.max(HIDDEN_SHARE);
    Ok(Verb::Resize {
        container: parent,
        shares,
    })
}

/// The layout gesture as the pipeline sees it: not an inventory verb (see the
/// module docs), but the same chain, so a gesture over HTTP is attributed and
/// audited like an agent's verb. The verb object is checked strictly by
/// [`parse_verb`] before it gets here, so `strict` is off to avoid checking
/// it twice.
static LAYOUT_APPLY: VerbDescriptor = VerbDescriptor {
    name: "layout-service_apply",
    service: "layout-service",
    method: "apply",
    description: "Apply one layout gesture (an `impress_layout::Verb`) from the host's \
                  `/api/layout/verb` route.",
    input_schema: apply_schema,
    output_schema: apply_schema,
    safety: Safety {
        class: SafetyClass::Mutating,
        idempotent: false,
    },
    since: "0.1.0",
    // The layout service's own set (its `impress_service_impl!` default).
    effects: Effects {
        reads: &[
            Kind::Ref("impress/ui/layout@1.0.0"),
            Kind::Ref("impress/ui/preset@1.0.0"),
        ],
        writes: &[Kind::Ref("impress/ui/layout@1.0.0")],
        reach: &[],
    },
    deprecated: None,
    aliases: &[],
    supports: &[],
    adjective: None,
    examples: &[],
    strict: false,
    budget_ms: None,
    replay_full: false,
    returns_kind: None,
    rows: None,
    secrets: &[],
    source: Source::Linked,
    handler: apply_not_callable,
};

fn apply_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verb": verb_schema(),
            "expected_revision": { "type": ["integer", "null"] },
        },
    })
}

fn apply_not_callable(_: Value) -> impress_service_core::ServiceFuture {
    Box::pin(async {
        Err(
            "layout-service_apply is the host's own gesture route, not a verb to call by name"
                .into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verb_is_parsed_strictly_with_an_optional_revision() {
        let (verb, expected) =
            parse_verb(br#"{"verb":"focus","target":{"role":"detail"},"expected_revision":4}"#)
                .unwrap();
        assert!(matches!(verb, Verb::Focus { .. }));
        assert_eq!(expected, Some(4));

        let unknown =
            parse_verb(br#"{"verb":"focus","target":{"role":"detail"},"bogus":1}"#).unwrap_err();
        assert_eq!(unknown.code, "invalid-argument");
        assert!(unknown.message.contains("bogus"), "{}", unknown.message);

        assert_eq!(parse_verb(b"nope").unwrap_err().code, "invalid-argument");
        assert_eq!(parse_verb(b"[1]").unwrap_err().code, "invalid-argument");
        assert!(
            parse_verb(br#"{"verb":"focus","target":{"role":"x"},"expected_revision":-1}"#)
                .is_err()
        );
    }
}
