//! Audit: one `core/verb-call@1.0.0` record per non-read-only call
//! (ADR-0036 D2; plan-verb-pipeline D-P3 as amended by
//! plan-self-reflective-layer D-R2, D-R3).
//!
//! The record is built here, after the envelope, from what the pipeline
//! knows: the verb, the caller, the trace and parent ids, the outcome, the
//! duration and a privacy-filtered argument summary (§ Call log's rule:
//! ids and sizes by default, short scalars by value, long strings as a
//! length and a hash, objects as their keys). It is then handed to the
//! installed [`Sink`], which owns the store write; this crate is on the
//! kit's pure tier and cannot open a store. `impress-store-service`
//! installs the sink — a bounded channel and one writer thread — through
//! [`crate::pipeline::Installer`], so any process that links the store
//! services records calls, and a process that does not counts them in
//! [`dropped`] rather than losing them silently.
//!
//! Read-only calls are not recorded (their timing is the span's) unless
//! `IMPRESS_CALL_LOG=all` is set for the session.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::{json, Map, Value};

use super::identity::CallerIdentity;
use crate::descriptor::VerbDescriptor;

/// The canonical spelling of the call record's kind.
pub const VERB_CALL_SCHEMA: &str = crate::schema_names::CORE_VERB_CALL;

/// Scalars longer than this are stored as a length and a hash.
pub const SCALAR_MAX: usize = 64;
/// Id lists longer than this are stored as a length and their first eight.
pub const ID_LIST_MAX: usize = 64;
/// Full replay arguments must serialize to strictly less than 16 KiB.
pub const REPLAY_ARG_MAX_BYTES: usize = 16 * 1024;

/// One call, as the audit layer records it.
#[derive(Debug, Clone)]
pub struct VerbCallRecord {
    /// The row id: the call id, which is also every written operation's
    /// `batch_id`.
    pub call_id: String,
    pub verb: std::borrow::Cow<'static, str>,
    pub since: std::borrow::Cow<'static, str>,
    pub caller: CallerIdentity,
    pub trace_id: String,
    pub parent_call: Option<String>,
    /// The privacy-filtered argument summary.
    pub args: Value,
    /// True only when `args` contains the exact original argument value.
    pub args_replayable: bool,
    /// Bounded, privacy-safe identifiers from the raw result, keyed by JSON path.
    pub result_ids: BTreeMap<String, Value>,
    /// True when an identifier candidate was omitted for privacy or bounds.
    pub result_ids_truncated: bool,
    /// UUIDs of items created and deleted by this call's handler.
    pub inserted_ids: Vec<String>,
    pub deleted_ids: Vec<String>,
    pub ok: bool,
    pub code: Option<String>,
    pub message_len: usize,
    /// RFC 3339.
    pub started_at: String,
    pub duration_ms: u64,
    pub arg_bytes: usize,
    pub result_bytes: usize,
    /// The per-call store override, when the call ran on one
    /// ([`super::invoke_on`]); the sink writes the row there.
    pub store_override: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// The name the caller actually asked for (P3, plan-verb-pipeline §
    /// Lifecycle), when an alias resolved to `verb`. `None` for a call by
    /// its canonical name — so a retired name's traffic is counted under
    /// the name that was actually asked for, not the one it now answers as.
    pub requested_name: Option<String>,
}

impl VerbCallRecord {
    /// The row's payload, as § Call log states it.
    pub fn payload(&self) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("verb".into(), json!(self.verb));
        payload.insert("since".into(), json!(self.since));
        payload.insert("caller".into(), self.caller.to_json());
        payload.insert("trace_id".into(), json!(self.trace_id));
        payload.insert("parent_call".into(), json!(self.parent_call));
        payload.insert("args".into(), self.args.clone());
        payload.insert("args_replayable".into(), json!(self.args_replayable));
        payload.insert("result_ids".into(), json!(self.result_ids));
        payload.insert(
            "result_ids_truncated".into(),
            json!(self.result_ids_truncated),
        );
        payload.insert("inserted_ids".into(), json!(self.inserted_ids));
        payload.insert("deleted_ids".into(), json!(self.deleted_ids));
        payload.insert("ok".into(), json!(self.ok));
        payload.insert("code".into(), json!(self.code));
        payload.insert("message_len".into(), json!(self.message_len));
        payload.insert("started_at".into(), json!(self.started_at));
        payload.insert("duration_ms".into(), json!(self.duration_ms));
        payload.insert("arg_bytes".into(), json!(self.arg_bytes));
        payload.insert("result_bytes".into(), json!(self.result_bytes));
        payload.insert("requested_name".into(), json!(self.requested_name));
        payload.insert("wire_version".into(), json!(crate::wire::WIRE_VERSION));
        payload
    }
}

/// Where records go. The sink must not block the caller: hand the record
/// to a bounded channel and return.
pub trait Sink: Send + Sync {
    fn record(&self, record: VerbCallRecord);
}

static SINK: RwLock<Option<Arc<dyn Sink>>> = RwLock::new(None);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static LOG_ALL: OnceLock<bool> = OnceLock::new();

/// Install (or replace) the process's sink.
pub fn install(sink: Arc<dyn Sink>) {
    if let Ok(mut slot) = SINK.write() {
        *slot = Some(sink);
    }
}

/// Whether a sink is installed.
pub fn has_sink() -> bool {
    SINK.read().ok().is_some_and(|s| s.is_some())
}

/// Records that reached no sink (none installed). A sink counts its own
/// overflow.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Count a record a sink could not keep.
pub fn count_dropped() {
    DROPPED.fetch_add(1, Ordering::Relaxed);
}

/// Whether read-only calls are recorded too (`IMPRESS_CALL_LOG=all`).
pub fn log_all() -> bool {
    *LOG_ALL.get_or_init(|| std::env::var("IMPRESS_CALL_LOG").as_deref() == Ok("all"))
}

/// Hand a record to the sink.
pub fn record(record: VerbCallRecord) {
    let sink = SINK.read().ok().and_then(|s| s.clone());
    match sink {
        Some(sink) => sink.record(record),
        None => {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                target: "verb",
                verb = %record.verb,
                call_id = %record.call_id,
                "no audit sink installed; call record dropped"
            );
        }
    }
}

/// Whether a field name is an id by the rule's spelling.
fn is_id_field(name: &str) -> bool {
    name == "id"
        || name == "ids"
        || name.ends_with("_id")
        || name.ends_with("_ids")
        || name == "cite_key"
        || name == "cite_keys"
}

/// Conservative schema check: an opt-in cannot persist full arguments when
/// any reachable schema branch declares a private value, including a nested
/// definition. This also covers a present private `null` whose summary would
/// otherwise happen to compare equal to the original input.
pub fn schema_contains_private(schema: &Value) -> bool {
    match schema {
        Value::Object(fields) => {
            fields.get("x-private") == Some(&Value::Bool(true))
                || fields.values().any(schema_contains_private)
        }
        Value::Array(items) => items.iter().any(schema_contains_private),
        _ => false,
    }
}

/// Choose the persisted arguments and state whether they are lossless.
pub fn recorded_args(args: &Value, schema: &Value, replay_full: bool) -> (Value, bool) {
    let private = schema_contains_private(schema);
    if replay_full
        && !private
        && serde_json::to_vec(args).is_ok_and(|bytes| bytes.len() < REPLAY_ARG_MAX_BYTES)
    {
        return (args.clone(), true);
    }
    let summary = summarize_args(args, schema);
    let replayable = !private && summary == *args;
    (summary, replayable)
}

fn resolved_schema<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let mut current = schema;
    for _ in 0..8 {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            break;
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            break;
        };
        let Some(next) = root.pointer(pointer) else {
            break;
        };
        current = next;
    }
    current
}

fn union_branch<'a>(branches: &'a [Value], value: &Value, root: &'a Value) -> Option<&'a Value> {
    let tag = value.get("kind").and_then(Value::as_str);
    let tagged: Vec<_> = branches
        .iter()
        .filter(|branch| {
            let Some(tag) = tag else {
                return false;
            };
            let branch = resolved_schema(branch, root);
            let Some(tag_schema) = branch.get("properties").and_then(|p| p.get("kind")) else {
                return false;
            };
            tag_schema.get("const").and_then(Value::as_str) == Some(tag)
                || tag_schema
                    .get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| choices.len() == 1 && choices[0].as_str() == Some(tag))
        })
        .collect();
    if tagged.len() == 1 {
        return Some(tagged[0]);
    }
    if !tagged.is_empty() {
        return None;
    }
    let typed: Vec<_> = branches
        .iter()
        .filter(|branch| {
            let branch = resolved_schema(branch, root);
            let Some(kind) = branch.get("type").and_then(Value::as_str) else {
                return false;
            };
            matches!(
                (kind, value),
                ("null", Value::Null)
                    | ("object", Value::Object(_))
                    | ("array", Value::Array(_))
                    | ("string", Value::String(_))
                    | ("boolean", Value::Bool(_))
                    | ("number" | "integer", Value::Number(_))
            )
        })
        .collect();
    (typed.len() == 1).then(|| typed[0])
}

fn schema_child<'a>(
    schema: &'a Value,
    value: &Value,
    root: &'a Value,
    key: &str,
) -> Result<Option<&'a Value>, ()> {
    let schema = resolved_schema(schema, root);
    if schema.get("$ref").is_some() {
        return Err(());
    }
    let direct = schema
        .get("properties")
        .and_then(|p| p.get(key))
        .or_else(|| key.parse::<usize>().ok().and_then(|_| schema.get("items")));
    if direct.is_some() {
        return Ok(direct);
    }
    if let Some(additional) = schema.get("additionalProperties") {
        return additional.as_object().map(|_| Some(additional)).ok_or(());
    }
    if schema.get("allOf").is_some() {
        return Err(());
    }
    for name in ["oneOf", "anyOf"] {
        if let Some(branches) = schema.get(name) {
            let branch = union_branch(branches.as_array().ok_or(())?, value, root).ok_or(())?;
            return schema_child(branch, value, root, key);
        }
    }
    Ok(None)
}

fn has_id_candidate(value: &Value, name: &str) -> bool {
    fn scan(value: &Value, name: &str, budget: &mut usize, depth: usize) -> bool {
        if *budget == 0 || depth > 16 {
            return true;
        }
        *budget -= 1;
        if is_id_field(name) && !value.is_null() {
            return true;
        }
        match value {
            Value::Object(fields) => fields
                .iter()
                .any(|(key, child)| scan(child, key, budget, depth + 1)),
            Value::Array(items) => items
                .iter()
                .any(|child| scan(child, name, budget, depth + 1)),
            _ => false,
        }
    }
    scan(value, name, &mut 1024, 0)
}

fn safe_id(value: &Value) -> bool {
    value.as_u64().is_some()
        || value.as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-+@".contains(&b))
        })
}

/// Inspect schema constraints against the finite result value. Recursive
/// render-tree schemas are safe when the actual result terminates; a cycle
/// that does not consume a value node, an unresolved reference, or exhausted
/// work budget still fails closed. A union checks every branch because we
/// cannot prove which branch governed a returned value.
fn reachable_private_or_unknown(
    schema: &Value,
    value: &Value,
    root: &Value,
    budget: &mut usize,
    depth: usize,
) -> bool {
    if *budget == 0 || depth >= 64 {
        return true;
    }
    *budget -= 1;
    let Some(fields) = schema.as_object() else {
        return false;
    };
    if fields.get("x-private") == Some(&Value::Bool(true)) {
        return true;
    }
    if let Some(reference) = fields.get("$ref") {
        let Some(pointer) = reference.as_str().and_then(|s| s.strip_prefix('#')) else {
            return true;
        };
        let Some(target) = root.pointer(pointer) else {
            return true;
        };
        if reachable_private_or_unknown(target, value, root, budget, depth + 1) {
            return true;
        }
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = fields.get(key) {
            let Some(branches) = branches.as_array() else {
                return true;
            };
            if branches
                .iter()
                .any(|branch| reachable_private_or_unknown(branch, value, root, budget, depth + 1))
            {
                return true;
            }
        }
    }
    if let Some(properties) = fields.get("properties").and_then(Value::as_object) {
        if let Some(actual) = value.as_object() {
            for (key, child) in actual {
                if let Some(child_schema) = properties.get(key) {
                    if reachable_private_or_unknown(child_schema, child, root, budget, depth + 1) {
                        return true;
                    }
                } else if let Some(additional) = fields.get("additionalProperties") {
                    if reachable_private_or_unknown(additional, child, root, budget, depth + 1) {
                        return true;
                    }
                }
            }
        }
    } else if let (Some(additional), Some(actual)) =
        (fields.get("additionalProperties"), value.as_object())
    {
        if actual
            .values()
            .any(|child| reachable_private_or_unknown(additional, child, root, budget, depth + 1))
        {
            return true;
        }
    }
    if let (Some(items), Some(actual)) = (fields.get("items"), value.as_array()) {
        if actual
            .iter()
            .any(|child| reachable_private_or_unknown(items, child, root, budget, depth + 1))
        {
            return true;
        }
    }
    false
}

fn output_schema_private(schema: &Value, value: &Value, root: &Value) -> bool {
    fn inspect(schema: &Value, value: &Value, root: &Value, active_refs: &mut Vec<String>) -> bool {
        let Some(fields) = schema.as_object() else {
            return false;
        };
        if fields.get("x-private") == Some(&Value::Bool(true)) {
            return true;
        }
        if let Some(reference) = fields.get("$ref") {
            let Some(reference) = reference.as_str() else {
                return true;
            };
            let Some(pointer) = reference.strip_prefix('#') else {
                return true;
            };
            if active_refs.iter().any(|seen| seen == reference) || active_refs.len() >= 16 {
                return true;
            }
            let Some(target) = root.pointer(pointer) else {
                return true;
            };
            active_refs.push(reference.to_owned());
            let unsafe_ref = inspect(target, value, root, active_refs);
            active_refs.pop();
            if unsafe_ref {
                return true;
            }
            // A `$ref` may have active sibling constraints. The referenced
            // object's ordinary properties are visited below by IdCollector;
            // siblings need their own privacy check because schema_child
            // chooses the referenced property schema.
            let siblings: serde_json::Map<_, _> = fields
                .iter()
                .filter(|(key, _)| key.as_str() != "$ref" && key.as_str() != "$defs")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if !siblings.is_empty() {
                let mut budget = 1024;
                if reachable_private_or_unknown(
                    &Value::Object(siblings),
                    value,
                    root,
                    &mut budget,
                    0,
                ) {
                    return true;
                }
            }
        }
        let mut budget = 1024;
        ["allOf", "anyOf", "oneOf"].iter().any(|key| {
            fields.get(*key).is_some_and(|branches| {
                branches.as_array().is_none_or(|branches| {
                    branches.iter().any(|branch| {
                        reachable_private_or_unknown(branch, value, root, &mut budget, 0)
                    })
                })
            })
        })
    }
    inspect(schema, value, root, &mut Vec::new())
}

struct IdCollector<'a> {
    root: &'a Value,
    ids: BTreeMap<String, Value>,
    truncated: bool,
    visited: usize,
}

impl<'a> IdCollector<'a> {
    fn visit(
        &mut self,
        value: &Value,
        schema: Option<&'a Value>,
        path: &str,
        name: &str,
        depth: usize,
    ) {
        if depth > 16 || self.ids.len() >= ID_LIST_MAX || self.visited >= 1024 {
            self.truncated = true;
            return;
        }
        self.visited += 1;
        // An embedded unconstrained JSON value (such as a table row or plot
        // spec) is not a schema-backed identifier source. An explicit ID
        // inside it is omitted and makes the recording refuse, not captured.
        if path != "$"
            && schema.is_some_and(|s| {
                s == &Value::Bool(true) || s.as_object().is_some_and(|o| o.is_empty())
            })
        {
            self.truncated |= has_id_candidate(value, name);
            return;
        }
        if schema.is_some_and(|s| output_schema_private(s, value, self.root)) {
            self.truncated = true;
            return;
        }
        if is_id_field(name) && !matches!(value, Value::Array(_)) {
            if value.is_null() {
                return;
            }
            if safe_id(value) {
                self.ids.insert(path.to_owned(), value.clone());
            } else {
                self.truncated = true;
            }
            return;
        }
        match value {
            Value::Object(fields) => {
                for (key, child) in fields {
                    if self.ids.len() >= ID_LIST_MAX || self.visited >= 1024 {
                        self.truncated = true;
                        break;
                    }
                    if key.is_empty()
                        || key.len() > 64
                        || path.len() + key.len() + 1 > 256
                        || key.contains('.')
                        || key.contains('$')
                    {
                        self.truncated = true;
                        continue;
                    }
                    let child_schema = match schema.map(|s| schema_child(s, value, self.root, key))
                    {
                        Some(Err(())) => {
                            self.truncated |= has_id_candidate(child, key);
                            continue;
                        }
                        Some(Ok(child_schema)) => child_schema,
                        None => None,
                    };
                    self.visit(
                        child,
                        child_schema,
                        &format!("{path}.{key}"),
                        key,
                        depth + 1,
                    );
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    if self.ids.len() >= ID_LIST_MAX || self.visited >= 1024 {
                        self.truncated = true;
                        break;
                    }
                    let key = index.to_string();
                    let child_schema = match schema.map(|s| schema_child(s, value, self.root, &key))
                    {
                        Some(Err(())) => {
                            self.truncated |= has_id_candidate(child, name);
                            continue;
                        }
                        Some(Ok(child_schema)) => child_schema,
                        None => None,
                    };
                    self.visit(
                        child,
                        child_schema,
                        &format!("{path}.{index}"),
                        name,
                        depth + 1,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Extract only named identifiers from the raw result. No body, array, or
/// arbitrary scalar is persisted. Paths use the same `$.field.0.id` spelling
/// as scenario call outcomes, and output is capped at 64 entries. The flag
/// prevents a recorder from treating an incomplete map as a safe template.
pub fn result_ids(result: &Value, output_schema: &Value) -> (BTreeMap<String, Value>, bool) {
    let mut collector = IdCollector {
        root: output_schema,
        ids: BTreeMap::new(),
        truncated: false,
        visited: 0,
    };
    let root_is_uuid = resolved_schema(output_schema, output_schema)
        .get("format")
        .and_then(Value::as_str)
        == Some("uuid");
    if root_is_uuid && output_schema_private(output_schema, result, output_schema) {
        return (collector.ids, true);
    }
    if root_is_uuid {
        if result
            .as_str()
            .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        {
            collector.ids.insert("$".into(), result.clone());
            return (collector.ids, false);
        }
        return (collector.ids, true);
    }
    collector.visit(result, Some(output_schema), "$", "", 0);
    (collector.ids, collector.truncated)
}

fn hashed(s: &str) -> Value {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    json!({ "len": s.chars().count(), "sha256_8": hex })
}

fn scalar(value: &Value) -> Value {
    match value {
        Value::String(s) if s.chars().count() > SCALAR_MAX => hashed(s),
        other => other.clone(),
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

/// One field's summary.
fn summarize_field(name: &str, value: &Value) -> Value {
    match value {
        Value::Array(items) if is_id_field(name) && items.iter().all(is_scalar) => {
            if items.len() <= ID_LIST_MAX {
                Value::Array(items.iter().map(scalar).collect())
            } else {
                json!({ "len": items.len(), "first": items.iter().take(8).map(scalar).collect::<Vec<_>>() })
            }
        }
        Value::Array(items) => {
            let keys: Vec<&String> = items
                .iter()
                .find_map(Value::as_object)
                .map(|o| o.keys().collect())
                .unwrap_or_default();
            if keys.is_empty() {
                json!({ "len": items.len() })
            } else {
                json!({ "len": items.len(), "keys": keys })
            }
        }
        Value::Object(fields) => {
            json!({ "len": fields.len(), "keys": fields.keys().collect::<Vec<_>>() })
        }
        other => scalar(other),
    }
}

/// The privacy-filtered summary of an argument object (§ Call log's table).
/// `x-private` (H-P1-2) is honoured when the schema marks a property so;
/// until P1's attribute lands, no property is.
pub fn summarize_args(args: &Value, input_schema: &Value) -> Value {
    let Some(fields) = args.as_object() else {
        return summarize_field("", args);
    };
    let properties = input_schema.get("properties").and_then(Value::as_object);
    let private = |name: &str| -> bool {
        properties
            .and_then(|p| p.get(name))
            .and_then(|s| s.get("x-private"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let mut out = Map::new();
    for (name, value) in fields {
        let summary = if private(name) {
            match value {
                Value::String(s) => hashed(s),
                Value::Null => Value::Null,
                other => hashed(&other.to_string()),
            }
        } else {
            summarize_field(name, value)
        };
        out.insert(name.clone(), summary);
    }
    Value::Object(out)
}

/// An estimate of a value's serialized size, without serializing it: the
/// span's `arg_bytes` and `result_bytes`. Strings count their bytes, numbers
/// and literals a fixed width, containers their punctuation.
pub fn approx_size(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(_) => 5,
        Value::Number(_) => 3,
        Value::String(s) => s.len() + 2,
        Value::Array(items) => 2 + items.iter().map(|v| approx_size(v) + 1).sum::<usize>(),
        Value::Object(fields) => {
            2 + fields
                .iter()
                .map(|(k, v)| k.len() + 4 + approx_size(v))
                .sum::<usize>()
        }
    }
}

/// The verb descriptor's input schema, for [`summarize_args`].
pub fn schema_of(verb: &VerbDescriptor) -> Value {
    (verb.input_schema)()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_short_scalars_keep_their_values() {
        let args = json!({
            "id": "abc",
            "publication_ids": ["a", "b"],
            "cite_key": "smith2020",
            "limit": 10,
            "starred": true,
            "name": "short",
        });
        let summary = summarize_args(&args, &json!({}));
        assert_eq!(summary, args);
    }

    #[test]
    fn long_strings_bodies_and_objects_are_sizes_and_keys() {
        let long = "x".repeat(100);
        let args = json!({
            "text": long,
            "spec": {"surface": "s", "root": {}},
            "items": [{"a": 1}, {"a": 2}],
            "ids": (0..70).map(|i| i.to_string()).collect::<Vec<_>>(),
        });
        let summary = summarize_args(&args, &json!({}));
        assert_eq!(summary["text"]["len"], 100);
        assert_eq!(summary["text"]["sha256_8"].as_str().unwrap().len(), 8);
        assert_eq!(
            summary["spec"],
            json!({"len": 2, "keys": ["root", "surface"]})
        );
        assert_eq!(summary["items"], json!({"len": 2, "keys": ["a"]}));
        assert_eq!(summary["ids"]["len"], 70);
        assert_eq!(summary["ids"]["first"].as_array().unwrap().len(), 8);
        assert!(!summary.to_string().contains(&"x".repeat(65)));
    }

    #[test]
    fn a_private_field_never_appears_by_value() {
        let schema = json!({"properties": {"token": {"type": "string", "x-private": true}}});
        let summary = summarize_args(&json!({"token": "secret"}), &schema);
        assert!(!summary.to_string().contains("secret"));
        assert_eq!(summary["token"]["len"], 6);
    }

    #[test]
    fn replay_opt_in_is_lossless_only_below_the_size_limit() {
        let schema = json!({"type": "object", "properties": {"spec": {"type": "object"}}});
        let args = json!({"spec": {"nested": [1, 2, 3]}});
        assert_ne!(recorded_args(&args, &schema, false).0, args);
        assert_eq!(recorded_args(&args, &schema, true), (args, true));
        let exact = json!({"text": "x".repeat(REPLAY_ARG_MAX_BYTES - 11)});
        assert_eq!(
            serde_json::to_vec(&exact).unwrap().len(),
            REPLAY_ARG_MAX_BYTES
        );
        assert!(!recorded_args(&exact, &schema, true).1);
    }

    #[test]
    fn nested_private_and_private_null_refuse_replay() {
        let schema = json!({"type": "object", "properties": {
            "outer": {"$ref": "#/$defs/Outer"}
        }, "$defs": {"Outer": {"type": "object", "properties": {
            "secret": {"type": "string", "x-private": true}
        }}}});
        let args = json!({"outer": {"secret": "secret"}});
        let (stored, replayable) = recorded_args(&args, &schema, true);
        assert!(!replayable);
        assert!(!stored.to_string().contains(":\"secret\""));
        let null_schema = json!({"properties": {"token": {"x-private": true}}});
        let (stored, replayable) = recorded_args(&json!({"token": null}), &null_schema, true);
        assert_eq!(stored, json!({"token": null}));
        assert!(!replayable);
    }

    #[test]
    fn result_ids_follow_raw_paths_and_private_schema() {
        let schema = json!({"type": "object", "properties": {
            "items": {"type": "array", "items": {"$ref": "#/$defs/Item"}},
            "token_id": {"type": "string", "x-private": true},
            "body": {"type": "string"}
        }, "$defs": {"Item": {"type": "object", "properties": {
            "id": {"type": "string"},
            "secret_id": {"type": "string", "x-private": true}
        }}}});
        let raw = json!({"items": [{"id": "abc-123", "secret_id": "hidden"}],
            "token_id": "hidden", "body": "not-an-id", "ids": ["safe-1", "has spaces"]});
        let (ids, truncated) = result_ids(&raw, &schema);
        assert!(truncated, "private and unsafe identifiers were omitted");
        assert_eq!(ids.get("$.items.0.id"), Some(&json!("abc-123")));
        assert_eq!(ids.get("$.ids.0"), Some(&json!("safe-1")));
        assert!(!ids.contains_key("$.ids.1"));
        assert!(!ids.contains_key("$.items.0.secret_id"));
        assert!(!ids.contains_key("$.token_id"));
        assert!(!ids.contains_key("$.body"));
        let uuid = json!("550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            result_ids(&uuid, &json!({"format": "uuid"})).0.get("$"),
            Some(&uuid)
        );
        assert_eq!(
            result_ids(&uuid, &json!({"format": "uuid", "x-private": true})),
            (BTreeMap::new(), true)
        );
        assert_eq!(
            result_ids(&json!({"id": 42}), &json!({})).0.get("$.id"),
            Some(&json!(42))
        );
        let many = json!({"ids": (0..70).collect::<Vec<_>>()});
        let (ids, truncated) = result_ids(&many, &json!({}));
        assert_eq!(ids.len(), ID_LIST_MAX);
        assert!(truncated);
        let (ids, truncated) = result_ids(&json!({"ambiguous.path": {"id": "x"}}), &json!({}));
        assert!(ids.is_empty());
        assert!(truncated);
    }

    #[test]
    fn union_references_and_ref_siblings_cannot_expose_private_ids() {
        let schema = json!({"type": "object", "properties": {
            "public_id": {"type": "string"},
            "union": {"anyOf": [{"$ref": "#/$defs/PrivateObject"}]},
            "sibling": {"$ref": "#/$defs/PublicObject", "properties": {
                "id": {"x-private": true}
            }},
            "sibling_dynamic": {"$ref": "#/$defs/PublicObject",
                "additionalProperties": {"x-private": true}},
            "all_of": {"allOf": [{"properties": {"id": {"type": "string"}}}]},
            "unknown": {"$ref": "#/$defs/Missing"},
            "cycle": {"$ref": "#/$defs/Cycle"}
        }, "$defs": {
            "PrivateObject": {"type": "object", "properties": {
                "id": {"type": "string", "x-private": true}
            }},
            "PublicObject": {"type": "object", "properties": {
                "id": {"type": "string"}
            }},
            "Cycle": {"$ref": "#/$defs/Cycle"}
        }});
        let raw = json!({"public_id": "safe-1", "union": {"id": "hidden-1"},
            "sibling": {"id": "hidden-2"},
            "sibling_dynamic": {"id": "hidden-dynamic"},
            "all_of": {"id": "hidden-all"},
            "unknown": {"id": "hidden-3"},
            "cycle": {"id": "hidden-4"}});
        let (ids, truncated) = result_ids(&raw, &schema);
        assert_eq!(ids.get("$.public_id"), Some(&json!("safe-1")));
        assert_eq!(ids.len(), 1, "{ids:?}");
        assert!(truncated);
    }

    #[test]
    fn finite_recursive_render_tree_extracts_ids_without_a_false_cycle() {
        // SurfaceDispatchResult -> RenderTree -> RenderNode -> RenderKind's
        // tagged oneOf -> children RenderNode is a recursive *schema*, but
        // this returned tree is finite. Traversal must consume value nodes.
        let schema = json!({"$ref": "#/$defs/Reply", "$defs": {
            "Reply": {"type": "object", "properties": {
                "ok": {"type": "boolean"},
                "tree": {"$ref": "#/$defs/Node"}
            }},
            "Node": {"type": "object", "properties": {
                "id": {"type": "string"},
                "node": {"$ref": "#/$defs/Kind"}
            }},
            "Kind": {"oneOf": [
                {"type": "object", "properties": {
                    "kind": {"const": "column"},
                    "items": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
                }},
                {"type": "object", "properties": {
                    "kind": {"const": "button"},
                    "label": {"type": "string"}
                }}
            ]}
        }});
        let raw = json!({"ok": true, "tree": {"id": "root", "node": {
            "kind": "column", "items": [
                {"id": "star-paper", "node": {"kind": "button", "label": "Star"}},
                {"id": "tag-paper", "node": {"kind": "button", "label": "Tag"}}
            ]
        }}});
        let (ids, truncated) = result_ids(&raw, &schema);
        assert!(!truncated, "finite tree was marked incomplete: {ids:?}");
        assert_eq!(
            ids.get("$.tree.node.items.0.id"),
            Some(&json!("star-paper"))
        );
        assert_eq!(ids.get("$.tree.node.items.1.id"), Some(&json!("tag-paper")));
    }

    #[test]
    fn recursive_union_still_refuses_private_or_untyped_identifiers() {
        let schema = json!({"$ref": "#/$defs/Node", "$defs": {
            "Node": {"properties": {
                "id": {"type": "string"},
                "node": {"oneOf": [
                    {"properties": {
                        "kind": {"const": "column"},
                        "items": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
                    }},
                    {"properties": {
                        "kind": {"const": "button"},
                        "value": {},
                        "private": {"anyOf": [{"$ref": "#/$defs/Private"}]}
                    }}
                ]}
            }},
            "Private": {"properties": {"secret_id": {"x-private": true}}}
        }});
        let untyped = json!({"id": "root", "node": {"kind": "button", "value": {"id": "secret"}}});
        let (ids, truncated) = result_ids(&untyped, &schema);
        assert!(truncated);
        assert!(!ids.values().any(|value| value == "secret"));
        let private = json!({"id": "root", "node": {"kind": "button",
            "private": {"secret_id": "secret"}}});
        let (ids, truncated) = result_ids(&private, &schema);
        assert!(truncated);
        assert!(!ids.values().any(|value| value == "secret"));
    }

    #[test]
    fn approx_size_tracks_serialized_size() {
        let v = json!({"a": "hello", "b": [1, 2, 3], "c": null, "d": {"e": "x".repeat(200)}});
        let exact = v.to_string().len();
        let approx = approx_size(&v);
        assert!(approx.abs_diff(exact) * 10 < exact, "{approx} vs {exact}");
    }

    #[test]
    fn without_a_sink_records_are_counted_not_lost_silently() {
        let before = dropped();
        record(VerbCallRecord {
            call_id: "c".into(),
            verb: "t-service_x".into(),
            since: "0.1.0".into(),
            caller: CallerIdentity::Person,
            trace_id: "t".into(),
            parent_call: None,
            args: Value::Null,
            args_replayable: false,
            result_ids: BTreeMap::new(),
            result_ids_truncated: false,
            inserted_ids: vec![],
            deleted_ids: vec![],
            ok: true,
            code: None,
            message_len: 0,
            started_at: String::new(),
            duration_ms: 0,
            arg_bytes: 0,
            result_bytes: 0,
            store_override: None,
            requested_name: None,
        });
        assert!(dropped() > before || has_sink());
    }
}
