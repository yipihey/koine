//! `reduce(&spec, &state, &params, &sources, &Event) -> Result<(Value, Vec<Effect>)>`:
//! what the human did, turned into a new state plus the side effects the
//! runtime still needs to carry out.
//!
//! `sources` is the value of every source as the last render saw it (the
//! runtime's cache), so an action may reference `{{source.papers.0.id}}` in
//! its `args` or `payload` exactly as a node may (review RS-S8: before, reduce
//! saw an empty source map and every such reference failed at dispatch
//! although `validate` passed it).
//!
//! This function is pure: a [`Action::Call`] is never invoked here, only
//! template-resolved into an [`Effect::Call`] for `impress-surface-service`'s
//! `Executor` (S4) to run. The same goes for `publish`/`emit`/`open`/`refresh`.
//! The one action `reduce` *does* perform itself is [`Action::Set`] — writing
//! into the returned state directly — because state is the value this function
//! already owns and returns; there is nothing for an `Effect::Set` to hand back
//! to a caller that `reduce`'s own return value doesn't already carry.
//!
//! # `each`: one effect per selected id (wave 5, V5)
//!
//! A widget's event shape stays uniform — a `select` on a table or list is
//! always an array of ids — while a verb keeps its own natural signature (a
//! triage verb takes one `id: String`, never a list). [`Action::Call`] and
//! [`Action::Emit`] close that gap with `each`, a literal path (`run_action`
//! resolves it once, with [`Context::with_item`] unbound) that must name an
//! array; this function then runs that one action once per element, with
//! `item` bound to it for the duration of that element's `args`/`payload`
//! resolution, appending one [`Effect`] per element in order. An empty array
//! produces no effects and is not an error — see [`resolve_each`]. This is a
//! fan-out declaration, not a loop the spec computes with: there is still
//! nothing here but a path lookup and a `for`, exactly the shape ADR-0033 D3
//! already allows a runtime (never a spec) to have.
//!
//! # Finding the node an event names
//!
//! An [`Event::widget`] is an id exactly as [`crate::resolve::resolve`] assigned
//! it — author-given or `n0.2.1`-style auto-derived. `reduce` calls the same
//! [`crate::spec::walk_with_ids`] `resolve` and `validate` use, so a widget id a
//! renderer reports always finds the node that produced it, with no separate
//! bookkeeping to keep in step.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::spec::{
    walk_with_ids, Action, Event, EventKind, FieldKind, Node, NodeKind, SurfaceSpec,
};
use crate::state_path::{self, StatePathError};
use crate::template::{resolve_value, Context, Template, TemplateError};

/// A side effect `reduce` decided should happen but does not perform itself —
/// every action kind except `set`, which is folded into the returned state
/// directly (see the module docs).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    Call {
        verb: String,
        args: Value,
        into: Option<String>,
        /// Resolved adverbs, opaque to this layer (`docs/grammar.md` § Adverbs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adverbs: Option<Value>,
        /// What the handler does after this call has landed (plan-koine-live
        /// L-D5). Set only when the call has an `into` and actions follow it
        /// in the handler: they may read the call's result, which exists only
        /// once the runtime has executed it and written `into`, so `reduce`
        /// stops there and leaves them here, unrun. The runtime runs them
        /// with [`run_then`] against the state it then holds, and only if the
        /// call succeeded. With `each`, they ride on the last call of the
        /// fan-out: they run once, after the whole fan-out. A fan-out of no
        /// elements has no call to wait for, so they run at once, in
        /// `reduce`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        then: Vec<Action>,
    },
    /// `ids` is the resolved value to publish: either the literal path the
    /// action named (walked the same way `bind` is), or — when the action gave
    /// none — the triggering event's own `value`, which is what
    /// `{"publish": {}}` on a table/list `on_select` means in the plan's worked
    /// example.
    Publish {
        ids: Value,
    },
    Emit {
        name: String,
        payload: Value,
    },
    Open {
        query: Value,
        view_kind: String,
        target: Option<String>,
    },
    Refresh {
        source: String,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReduceError {
    #[error("event names widget '{widget}', which is not in this surface")]
    UnknownWidget { widget: String },
    #[error(
        "widget '{widget}' has no `bind` to change (a `change` event only applies to a field)"
    )]
    NotBindable { widget: String },
    #[error("`bind`/`set` path '{path}' must start with 'state.'")]
    InvalidPath { path: String },
    /// [`Action::Call`]/[`Action::Emit`]'s `each` resolved to something other
    /// than a JSON array — the one runtime check `validate` cannot make
    /// statically (it only knows the path's root, never its value).
    #[error("`each` path '{path}' did not resolve to an array")]
    EachNotArray { path: String },
    /// A `bind`, `set.path` or `publish.ids` path that could not be read or
    /// written — see [`crate::state_path`].
    #[error(transparent)]
    StatePath(#[from] StatePathError),
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// An action's resolved value is not the shape the action takes
    /// (`append` of something that is neither rows nor an array, onto
    /// something that is not an array).
    #[error("{message}")]
    InvalidArgument { message: String },
}

impl ReduceError {
    /// The stable, machine-readable name of this refusal — the `code` a
    /// dispatch result carries next to its message (review AC-F19).
    pub fn code(&self) -> &'static str {
        match self {
            ReduceError::UnknownWidget { .. } => "unknown-widget",
            ReduceError::NotBindable { .. } => "not-bindable",
            ReduceError::InvalidPath { .. } => "invalid-path",
            ReduceError::EachNotArray { .. } => "each-not-array",
            ReduceError::StatePath(e) => e.code(),
            ReduceError::InvalidArgument { .. } => "invalid-argument",
            ReduceError::Template(TemplateError::UnknownRoot { .. }) => "unknown-template-root",
            ReduceError::Template(TemplateError::MissingPath { .. }) => "missing-template-path",
        }
    }
}

/// See the module docs.
pub fn reduce(
    spec: &SurfaceSpec,
    state: &Value,
    params: &Value,
    sources: &Value,
    event: &Event,
) -> Result<(Value, Vec<Effect>), ReduceError> {
    let index: BTreeMap<String, &Node> = walk_with_ids(&spec.root).into_iter().collect();
    let node = index
        .get(event.widget.as_str())
        .ok_or_else(|| ReduceError::UnknownWidget {
            widget: event.widget.clone(),
        })?;

    let mut new_state = state.clone();
    let mut effects = Vec::new();
    let event_value = event_json(event);

    match event.kind {
        EventKind::Change => {
            // A tree keeps its bind in its own body (`tree.bind`, the expanded
            // ids); a disclosure change writes there exactly as a field's does.
            let bind = node
                .bind
                .clone()
                .or_else(|| match &node.kind {
                    NodeKind::Tree(t) => t.bind.clone(),
                    _ => None,
                })
                .ok_or_else(|| ReduceError::NotBindable {
                    widget: event.widget.clone(),
                })?;
            state_path::write(&mut new_state, &bind, change_value(node, &event.value))?;
            run_actions(
                &node.on_change,
                &mut new_state,
                (params, sources),
                &event_value,
                &mut effects,
            )?;
        }
        EventKind::Click => {
            // `button`, and (plan-koine-tree-stream T-D4) `plot` and `image`
            // declare `on_click`; the last two see `{{event.value.x}}` and
            // `{{event.value.y}}`.
            let on_click: &[Action] = match &node.kind {
                NodeKind::Button(b) => &b.on_click,
                NodeKind::Plot(p) => &p.on_click,
                NodeKind::Image(i) => &i.on_click,
                // A click on anything else is a no-op.
                _ => &[],
            };
            run_actions(
                on_click,
                &mut new_state,
                (params, sources),
                &event_value,
                &mut effects,
            )?;
        }
        EventKind::Select => match &node.kind {
            NodeKind::Table(t) => {
                run_actions(
                    &t.on_select,
                    &mut new_state,
                    (params, sources),
                    &event_value,
                    &mut effects,
                )?;
            }
            NodeKind::List(l) => {
                run_actions(
                    &l.on_select,
                    &mut new_state,
                    (params, sources),
                    &event_value,
                    &mut effects,
                )?;
            }
            NodeKind::Tree(t) => {
                run_actions(
                    &t.on_select,
                    &mut new_state,
                    (params, sources),
                    &event_value,
                    &mut effects,
                )?;
            }
            NodeKind::Graph(g) => {
                run_actions(
                    &g.on_select,
                    &mut new_state,
                    (params, sources),
                    &event_value,
                    &mut effects,
                )?;
            }
            _ => {}
        },
        // plan-koine-outline O-D3/O-D4: a tree's own row events. Each runs
        // the tree's matching list with `{{event.value}}` as sent; a node
        // with no such list (or of another kind) ignores the event, as a
        // `select` on a table without `on_select` does.
        EventKind::Rename | EventKind::Delete | EventKind::Activate | EventKind::Move => {
            if let NodeKind::Tree(t) = &node.kind {
                let handler: &[Action] = match event.kind {
                    EventKind::Rename => &t.on_rename,
                    EventKind::Delete => &t.on_delete,
                    EventKind::Activate => &t.on_activate,
                    _ => &t.on_move,
                };
                // The value's shape is the renderer's; a move is at least an
                // object (`{ids, to, index}`), and only a tree that handles
                // it is held to that.
                if event.kind == EventKind::Move && !handler.is_empty() && !event.value.is_object()
                {
                    return Err(ReduceError::InvalidArgument {
                        message: format!(
                            "a `move` event's value must be an object \
                             {{\"ids\", \"to\", \"index\"}}, got {}",
                            crate::spec::json_type(&event.value)
                        ),
                    });
                }
                run_actions(
                    handler,
                    &mut new_state,
                    (params, sources),
                    &event_value,
                    &mut effects,
                )?;
            }
        }
        // O-D5: a row menu entry, on a tree, table or list. `value` is
        // `{"id", "action": <label>}`; the entry's `on_click` runs. A widget
        // that declares no menu ignores the event; one that declares a menu
        // refuses a label it does not have, naming the ones it does.
        EventKind::Menu => {
            let actions: &[crate::spec::MenuAction] = match &node.kind {
                NodeKind::Tree(t) => &t.actions,
                NodeKind::Table(t) => &t.actions,
                NodeKind::List(l) => &l.actions,
                _ => &[],
            };
            if !actions.is_empty() {
                let label = event.value.get("action").and_then(Value::as_str);
                let entry = label.and_then(|l| actions.iter().find(|a| a.label == l));
                let Some(entry) = entry else {
                    let labels: Vec<&str> = actions.iter().map(|a| a.label.as_str()).collect();
                    return Err(ReduceError::InvalidArgument {
                        message: match label {
                            Some(l) => format!(
                                "widget '{}' has no menu action '{l}'; its actions are: {}",
                                event.widget,
                                labels.join(", ")
                            ),
                            None => format!(
                                "a `menu` event's value needs an `action` label (a string); \
                                 widget '{}' has: {}",
                                event.widget,
                                labels.join(", ")
                            ),
                        },
                    });
                };
                run_actions(
                    &entry.on_click,
                    &mut new_state,
                    (params, sources),
                    &event_value,
                    &mut effects,
                )?;
            }
        }
        EventKind::Submit => {
            // "runs `on_submit` if declared else is a no-op" — an empty
            // `on_submit` list already does nothing, so no branch is needed.
            run_actions(
                &node.on_submit,
                &mut new_state,
                (params, sources),
                &event_value,
                &mut effects,
            )?;
        }
    }

    Ok((new_state, effects))
}

/// The `{{event.…}}` root: the event as a handler's templates see it.
fn event_json(event: &Event) -> Value {
    serde_json::json!({
        "widget": event.widget,
        "kind": event_kind_str(event.kind),
        "value": event.value,
    })
}

/// Run the actions an [`Effect::Call`] carried in `then`, after the runtime
/// has executed the call and written its `into` into `state`. `params` and
/// `sources` are the ones the handler saw, `event` the one that triggered it.
/// Returns the effects those actions decided on (a further call, a publish…),
/// for the runtime to run in order before whatever followed the first call;
/// a further call with an `into` carries the rest of `then` the same way.
pub fn run_then(
    state: &mut Value,
    then: &[Action],
    params: &Value,
    sources: &Value,
    event: &Event,
) -> Result<Vec<Effect>, ReduceError> {
    let mut effects = Vec::new();
    run_actions(
        then,
        state,
        (params, sources),
        &event_json(event),
        &mut effects,
    )?;
    Ok(effects)
}

/// What a `change` writes into state. A text field marked `raw_json` (the
/// generated form's field for an array or object argument,
/// `impress-verb-surface`) holds a JSON value, but every renderer's text input
/// sends the characters the person typed: a string that parses as JSON is
/// stored as the value it spells, so `[1, 2, 3]` typed into the field is the
/// array `[1, 2, 3]` when Run sends it. Text that is not JSON is stored as
/// typed, and the verb refuses it by name.
fn change_value(node: &Node, value: &Value) -> Value {
    let raw_json = matches!(
        &node.kind,
        NodeKind::Field(FieldKind::Text(options))
            if options.get("raw_json").and_then(Value::as_bool) == Some(true)
    );
    match value {
        Value::String(text) if raw_json => {
            serde_json::from_str(text).unwrap_or_else(|_| value.clone())
        }
        _ => value.clone(),
    }
}

fn event_kind_str(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Change => "change",
        EventKind::Click => "click",
        EventKind::Select => "select",
        EventKind::Submit => "submit",
        EventKind::Rename => "rename",
        EventKind::Delete => "delete",
        EventKind::Activate => "activate",
        EventKind::Move => "move",
        EventKind::Menu => "menu",
    }
}

/// `(params, sources)`: the two read-only roots every action sees.
type Roots<'a> = (&'a Value, &'a Value);

fn run_actions(
    actions: &[Action],
    state: &mut Value,
    roots: Roots<'_>,
    event_value: &Value,
    effects: &mut Vec<Effect>,
) -> Result<(), ReduceError> {
    for (index, action) in actions.iter().enumerate() {
        let before = effects.len();
        run_action(action, state, roots, event_value, effects)?;
        // A call that writes `into` is a point the rest of the handler must
        // wait at: see [`Effect::Call::then`].
        if matches!(action, Action::Call { into: Some(_), .. }) && effects.len() > before {
            let rest = &actions[index + 1..];
            if !rest.is_empty() {
                if let Some(Effect::Call { then, .. }) = effects.last_mut() {
                    *then = rest.to_vec();
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

fn run_action(
    action: &Action,
    state: &mut Value,
    (params, sources): Roots<'_>,
    event_value: &Value,
    effects: &mut Vec<Effect>,
) -> Result<(), ReduceError> {
    // Rebuilt before every action (rather than held across the loop) so a `set`
    // earlier in the same handler is visible to a `{{state.…}}` reference in a
    // later action, without holding a borrow of `state` across the write that
    // follows it. Sources are as the last render left them: they are not
    // re-run mid-reduce.
    let ctx = Context::new(state, params, sources, event_value);

    match action {
        Action::Set { path, value } => {
            let resolved = resolve_value(value, &ctx)?;
            state_path::write(state, path, resolved)?;
        }
        Action::Append { path, value } => {
            let resolved = resolve_value(value, &ctx)?;
            let added = rows_to_append(path, &resolved)?;
            let existing = match state_path::read(state, path) {
                Ok(existing) => Some(existing),
                Err(StatePathError::Missing { .. }) => None,
                Err(other) => return Err(other.into()),
            };
            let merged = match existing {
                None | Some(Value::Null) => added,
                Some(Value::Array(mut rows)) => {
                    rows.extend(added);
                    rows
                }
                Some(other) => {
                    return Err(ReduceError::InvalidArgument {
                        message: format!(
                            "`append` to '{path}': the target is {}, not an array",
                            crate::spec::json_type(&other)
                        ),
                    })
                }
            };
            state_path::write(state, path, Value::Array(merged))?;
        }
        Action::Call {
            verb,
            args,
            into,
            each,
            adverbs,
        } => match each {
            None => {
                let resolved_args = resolve_value(args, &ctx)?;
                let resolved_adverbs = adverbs
                    .as_ref()
                    .map(|a| resolve_value(a, &ctx))
                    .transpose()?;
                effects.push(Effect::Call {
                    verb: verb.clone(),
                    args: resolved_args,
                    into: into.clone(),
                    adverbs: resolved_adverbs,
                    then: Vec::new(),
                });
            }
            Some(each_path) => {
                for item in resolve_each(each_path, &ctx)? {
                    let item_ctx = ctx.with_item(&item);
                    let resolved_args = resolve_value(args, &item_ctx)?;
                    let resolved_adverbs = adverbs
                        .as_ref()
                        .map(|a| resolve_value(a, &item_ctx))
                        .transpose()?;
                    effects.push(Effect::Call {
                        verb: verb.clone(),
                        args: resolved_args,
                        into: into.clone(),
                        adverbs: resolved_adverbs,
                        then: Vec::new(),
                    });
                }
            }
        },
        Action::Publish { ids } => {
            let resolved = match ids {
                // A path that is not there fails the dispatch (review RS-S8)
                // — it used to publish `null`, i.e. clear the selection.
                Some(path) => state_path::read(state, path)?,
                None => event_value.get("value").cloned().unwrap_or(Value::Null),
            };
            effects.push(Effect::Publish { ids: resolved });
        }
        Action::Emit {
            name,
            payload,
            each,
        } => match each {
            None => {
                let resolved = resolve_value(payload, &ctx)?;
                effects.push(Effect::Emit {
                    name: name.clone(),
                    payload: resolved,
                });
            }
            Some(each_path) => {
                for item in resolve_each(each_path, &ctx)? {
                    let item_ctx = ctx.with_item(&item);
                    let resolved = resolve_value(payload, &item_ctx)?;
                    effects.push(Effect::Emit {
                        name: name.clone(),
                        payload: resolved,
                    });
                }
            }
        },
        Action::Open {
            query,
            view_kind,
            target,
        } => {
            let resolved_query = resolve_value(query, &ctx)?;
            effects.push(Effect::Open {
                query: resolved_query,
                view_kind: view_kind.clone(),
                target: target.clone(),
            });
        }
        Action::Refresh { source } => {
            effects.push(Effect::Refresh {
                source: source.clone(),
            });
        }
    }
    Ok(())
}

/// What `append` adds: the `rows` of a `{rows, next}` envelope, or the array
/// itself. Anything else is refused by shape (`invalid-argument`).
fn rows_to_append(path: &str, resolved: &Value) -> Result<Vec<Value>, ReduceError> {
    match crate::envelope::unwrap_rows(resolved).0 {
        Value::Array(rows) => Ok(rows.clone()),
        _ => Err(ReduceError::InvalidArgument {
            message: format!(
                "`append` to '{path}': the value is {}, not an array or a {{rows, next}} envelope",
                crate::spec::json_type(resolved)
            ),
        }),
    }
}

/// Resolve an `each` literal path (any of the four `Context` roots — not
/// `state.` only, unlike `bind`/`set`/`publish.ids`: `validate::validate`
/// checks this) against `ctx` and require the result to be a JSON array. Reuses
/// [`Template::Single`]'s resolution — and therefore its errors — rather than a
/// second path-walking implementation: an unknown root or a missing path is
/// reported through the same [`TemplateError`] channel `resolve_value` already
/// uses, wrapped into a [`ReduceError`] by the same `#[from]` conversion.
fn resolve_each(each_path: &str, ctx: &Context) -> Result<Vec<Value>, ReduceError> {
    let segments: Vec<String> = each_path.split('.').map(str::to_string).collect();
    let value = Template::Single(segments).resolve(ctx)?;
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(ReduceError::EachNotArray {
            path: each_path.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Button, Node, NodeKind, SurfaceSpec, SURFACE_VERSION};

    fn star_button_spec(state: Value) -> SurfaceSpec {
        let root = Node::leaf(NodeKind::Button(Button {
            label: "Star".to_string(),
            on_click: vec![Action::Call {
                verb: "triage-service_set-starred".to_string(),
                args: serde_json::json!({"id": "{{item}}", "starred": true}),
                into: None,
                each: Some("state.selected".to_string()),
                adverbs: None,
            }],
        }))
        .with_id("star-btn");
        SurfaceSpec {
            surface: SURFACE_VERSION.to_string(),
            name: "t".to_string(),
            params: Vec::new(),
            state,
            sources: BTreeMap::new(),
            root,
            extra: Default::default(),
        }
    }

    fn click() -> Event {
        Event {
            widget: "star-btn".to_string(),
            kind: EventKind::Click,
            value: Value::Null,
        }
    }

    #[test]
    fn each_fans_out_one_call_effect_per_element_with_item_bound() {
        let spec = star_button_spec(serde_json::json!({"selected": ["a", "b"]}));
        let params = Value::Null;
        let (_, effects) = reduce(&spec, &spec.state, &params, &Value::Null, &click()).unwrap();
        assert_eq!(
            effects,
            vec![
                Effect::Call {
                    verb: "triage-service_set-starred".to_string(),
                    args: serde_json::json!({"id": "a", "starred": true}),
                    into: None,
                    adverbs: None,
                    then: Vec::new(),
                },
                Effect::Call {
                    verb: "triage-service_set-starred".to_string(),
                    args: serde_json::json!({"id": "b", "starred": true}),
                    into: None,
                    adverbs: None,
                    then: Vec::new(),
                },
            ]
        );
    }

    fn adverb_spec(each: Option<&str>, adverbs: Value, state: Value) -> SurfaceSpec {
        let mut spec = star_button_spec(state);
        spec.root = Node::leaf(NodeKind::Button(Button {
            label: "Star".to_string(),
            on_click: vec![Action::Call {
                verb: "triage-service_set-starred".to_string(),
                args: serde_json::json!({"id": "{{item}}"}),
                into: None,
                each: each.map(str::to_string),
                adverbs: Some(adverbs),
            }],
        }))
        .with_id("star-btn");
        spec
    }

    #[test]
    fn adverbs_are_template_resolved_into_the_call_effect() {
        let mut spec = adverb_spec(
            None,
            serde_json::json!({"tentatively": true, "once": "k-{{state.id}}"}),
            serde_json::json!({"id": "7"}),
        );
        spec.root = Node::leaf(NodeKind::Button(Button {
            label: "Go".to_string(),
            on_click: vec![Action::Call {
                verb: "v_x".to_string(),
                args: serde_json::json!({}),
                into: None,
                each: None,
                adverbs: Some(serde_json::json!({"tentatively": true, "once": "k-{{state.id}}"})),
            }],
        }))
        .with_id("star-btn");
        let (_, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        match &effects[0] {
            Effect::Call { adverbs, .. } => assert_eq!(
                adverbs,
                &Some(serde_json::json!({"tentatively": true, "once": "k-7"}))
            ),
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn each_resolves_item_inside_adverbs_per_item() {
        let spec = adverb_spec(
            Some("state.selected"),
            serde_json::json!({"once": "star-{{item}}"}),
            serde_json::json!({"selected": ["a", "b"]}),
        );
        let (_, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        let got: Vec<_> = effects
            .iter()
            .map(|e| match e {
                Effect::Call { adverbs, .. } => adverbs.clone().unwrap(),
                other => panic!("expected Call, got {other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            vec![
                serde_json::json!({"once": "star-a"}),
                serde_json::json!({"once": "star-b"})
            ]
        );
    }

    #[test]
    fn an_action_without_adverbs_serialises_with_no_adverbs_key() {
        let a = Action::Call {
            verb: "v_x".to_string(),
            args: Value::Null,
            into: None,
            each: None,
            adverbs: None,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert!(!v.to_string().contains("adverbs"));
    }

    #[test]
    fn an_empty_each_array_produces_no_effects_and_is_not_an_error() {
        let spec = star_button_spec(serde_json::json!({"selected": []}));
        let params = Value::Null;
        let (_, effects) = reduce(&spec, &spec.state, &params, &Value::Null, &click()).unwrap();
        assert_eq!(effects, Vec::new());
    }

    #[test]
    fn a_non_array_each_is_a_reduce_error_naming_the_path() {
        let spec = star_button_spec(serde_json::json!({"selected": "not-an-array"}));
        let params = Value::Null;
        let err = reduce(&spec, &spec.state, &params, &Value::Null, &click()).unwrap_err();
        assert_eq!(
            err,
            ReduceError::EachNotArray {
                path: "state.selected".to_string()
            }
        );
    }

    #[test]
    fn each_fans_out_emit_the_same_way_as_call() {
        let root = Node::leaf(NodeKind::Button(Button {
            label: "Star".to_string(),
            on_click: vec![Action::Emit {
                name: "triaged".to_string(),
                payload: serde_json::json!({"id": "{{item}}"}),
                each: Some("state.selected".to_string()),
            }],
        }))
        .with_id("star-btn");
        let spec = SurfaceSpec {
            surface: SURFACE_VERSION.to_string(),
            name: "t".to_string(),
            params: Vec::new(),
            state: serde_json::json!({"selected": ["x", "y"]}),
            sources: BTreeMap::new(),
            root,
            extra: Default::default(),
        };
        let params = Value::Null;
        let (_, effects) = reduce(&spec, &spec.state, &params, &Value::Null, &click()).unwrap();
        assert_eq!(
            effects,
            vec![
                Effect::Emit {
                    name: "triaged".to_string(),
                    payload: serde_json::json!({"id": "x"}),
                },
                Effect::Emit {
                    name: "triaged".to_string(),
                    payload: serde_json::json!({"id": "y"}),
                },
            ]
        );
    }

    fn call_then_spec(into: Option<&str>, each: Option<&str>, state: Value) -> SurfaceSpec {
        let mut spec = star_button_spec(state);
        spec.root = Node::leaf(NodeKind::Button(Button {
            label: "Go".to_string(),
            on_click: vec![
                Action::Set {
                    path: "state.before".to_string(),
                    value: serde_json::json!(1),
                },
                Action::Call {
                    verb: "v_x".to_string(),
                    args: serde_json::json!({"id": if each.is_some() { "{{item}}" } else { "x" }}),
                    into: into.map(str::to_string),
                    each: each.map(str::to_string),
                    adverbs: None,
                },
                Action::Set {
                    path: "state.after".to_string(),
                    value: serde_json::json!("{{state.got}}"),
                },
            ],
        }))
        .with_id("star-btn");
        spec
    }

    #[test]
    fn the_actions_after_a_call_with_into_ride_on_its_effect_unrun() {
        let spec = call_then_spec(Some("state.got"), None, serde_json::json!({"got": null}));
        let (state, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        // Before the call: run now. After it: not yet.
        assert_eq!(state["before"], serde_json::json!(1));
        assert!(state.get("after").is_none(), "{state}");
        let Effect::Call { then, .. } = &effects[0] else {
            panic!("a call")
        };
        assert_eq!(then.len(), 1);

        // The runtime writes `into`, then runs the rest against that state.
        let mut state = state;
        state["got"] = serde_json::json!("page-2");
        let more = run_then(&mut state, then, &Value::Null, &Value::Null, &click()).unwrap();
        assert!(more.is_empty());
        assert_eq!(state["after"], serde_json::json!("page-2"));
    }

    #[test]
    fn a_call_without_into_or_with_nothing_after_it_carries_nothing() {
        let spec = call_then_spec(None, None, serde_json::json!({"got": 7}));
        let (state, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        assert_eq!(state["after"], serde_json::json!(7));
        assert!(matches!(&effects[0], Effect::Call { then, .. } if then.is_empty()));

        let mut spec = call_then_spec(Some("state.got"), None, serde_json::json!({"got": 7}));
        if let NodeKind::Button(b) = &mut spec.root.kind {
            b.on_click.pop();
        }
        let (_, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        assert!(matches!(&effects[0], Effect::Call { then, .. } if then.is_empty()));
    }

    #[test]
    fn under_each_the_rest_rides_on_the_last_call_and_an_empty_fan_out_runs_it_now() {
        let spec = call_then_spec(
            Some("state.got"),
            Some("state.selected"),
            serde_json::json!({"selected": ["a", "b"], "got": 1}),
        );
        let (state, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        assert!(state.get("after").is_none());
        assert_eq!(effects.len(), 2);
        assert!(matches!(&effects[0], Effect::Call { then, .. } if then.is_empty()));
        assert!(matches!(&effects[1], Effect::Call { then, .. } if then.len() == 1));

        let spec = call_then_spec(
            Some("state.got"),
            Some("state.selected"),
            serde_json::json!({"selected": [], "got": 1}),
        );
        let (state, effects) =
            reduce(&spec, &spec.state, &Value::Null, &Value::Null, &click()).unwrap();
        assert!(effects.is_empty());
        assert_eq!(state["after"], serde_json::json!(1));
    }

    #[test]
    fn a_raw_json_text_field_stores_the_json_typed_into_it() {
        use crate::spec::FieldKind;
        let field = |options: Value| {
            Node::leaf(NodeKind::Field(FieldKind::Text(options)))
                .with_id("f")
                .with_bind("state.v")
        };
        let change = |text: &str| Event {
            widget: "f".to_string(),
            kind: EventKind::Change,
            value: Value::String(text.to_string()),
        };
        let run = |options: Value, text: &str| {
            let mut spec = star_button_spec(serde_json::json!({"v": null}));
            spec.root = field(options);
            reduce(
                &spec,
                &spec.state,
                &Value::Null,
                &Value::Null,
                &change(text),
            )
            .unwrap()
            .0["v"]
                .clone()
        };
        let raw = serde_json::json!({"raw_json": true});
        assert_eq!(run(raw.clone(), "[1, 2, 3]"), serde_json::json!([1, 2, 3]));
        assert_eq!(run(raw.clone(), "{\"a\": 1}"), serde_json::json!({"a": 1}));
        // Not JSON: kept as typed, for the verb to refuse by name.
        assert_eq!(run(raw, "[1, 2"), serde_json::json!("[1, 2"));
        // An ordinary text field keeps its characters, JSON-looking or not.
        assert_eq!(
            run(serde_json::json!({}), "[1, 2]"),
            serde_json::json!("[1, 2]")
        );
    }
}
