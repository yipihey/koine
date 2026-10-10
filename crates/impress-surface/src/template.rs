//! The template language a spec's strings are written in (`docs/plan-agent-surfaces.md`
//! "The vocabulary (normative)": "a string that is exactly one `{{path}}` resolves
//! to the JSON value at that path, mixed text stringifies").
//!
//! There are five roots, each backed by one JSON value in a [`Context`]:
//!
//! | root     | resolves against                                         |
//! |----------|-----------------------------------------------------------|
//! | `state`  | the surface's current state                               |
//! | `param`  | the pane parameters bound to the surface                  |
//! | `source` | the most recently fetched value of each named source      |
//! | `event`  | the event being reduced (`reduce` only; empty otherwise)  |
//! | `item`   | the element `reduce`'s `each` fan-out is currently on     |
//!
//! This is deliberately not a language: a reference is a dotted path into one of
//! those roots, walked through `Value::Object` maps by key or `Value::Array`s by
//! a numeric segment (`{{state.selected.0}}` is JSON-Pointer-style indexing, not
//! an operator — see [`resolve_path`]) — no filters, no slicing, no arithmetic.
//! ADR-0033 D3 closes the vocabulary for the same reason ADR-0031 D2 closes the
//! pane-query algebra: a spec an agent can generate reliably and a document that
//! never needs a sandbox. Anything that needs to compute is a verb (D4), not a
//! richer template; fanning an action out over an array is a structural
//! declaration (`each`, `reduce.rs`), not a loop the spec itself computes with.
//!
//! # What is a reference, and what is text
//!
//! A `{{…}}` is a reference only when its first segment is one of the five
//! roots above AND every segment is a plain name (`[A-Za-z0-9_-]+`):
//! `{{state.bins}}`, `{{source.papers.0.title}}`, `{{item}}`. Anything else
//! between double braces is kept as literal text, braces included — so a
//! text node may hold LaTeX or Typst (`$\frac{{a}}{b}$`), a literal `{{`, or
//! a `{{` with no closing `}}`, and the human sees exactly what was written
//! (review RS-S10: before, every `{{…}}` was taken as a reference and a
//! formula became a whole-node placeholder). There is therefore no escape
//! syntax to learn; `validate` warns about a `{{a.b}}` whose root is not a
//! template root, since that is more likely a typo than text.
//!
//! [`Template::parse`] never fails. What *does* fail — at resolve time, not
//! parse time — is a path that is not present in its context
//! ([`TemplateError::MissingPath`]), or `{{item}}` outside an `each` fan-out
//! ([`TemplateError::UnknownRoot`]; see [`Context::with_item`]).

use std::fmt;

use serde_json::{Map, Value};

/// The JSON values a template can reference into. `event` is `Value::Null`
/// outside [`crate::reduce::reduce`] — `{{event.…}}` outside a reduce is simply a
/// path that is not present, i.e. a [`TemplateError::MissingPath`], not a special
/// case. `item` starts unbound ([`Context::new`]); [`Context::with_item`] binds
/// it for the duration of one `each` fan-out element (`reduce.rs`).
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub state: &'a Value,
    pub params: &'a Value,
    pub source: &'a Value,
    pub event: &'a Value,
    pub item: Option<&'a Value>,
    /// Why a source is ABSENT from `source`, by name — a verb that refused
    /// ("imbib is not running"), a query that did not compile. `resolve`
    /// names it in the placeholder a reference to that source becomes;
    /// nothing else reads it, and `None` (every caller but the runtime) keeps
    /// the generic "did not resolve" wording.
    pub source_errors: Option<&'a std::collections::BTreeMap<String, String>>,
}

impl<'a> Context<'a> {
    pub fn new(state: &'a Value, params: &'a Value, source: &'a Value, event: &'a Value) -> Self {
        Context {
            state,
            params,
            source,
            event,
            item: None,
            source_errors: None,
        }
    }

    /// Bind `item` for the returned context, leaving every other root as-is.
    /// `Context` is `Copy`, so this never disturbs the caller's own binding —
    /// [`crate::reduce::run_action`] calls it once per element of an `each`
    /// fan-out, from the same base context each time.
    pub fn with_item(mut self, item: &'a Value) -> Self {
        self.item = Some(item);
        self
    }

    fn root(&self, name: &str) -> Option<&'a Value> {
        match name {
            "state" => Some(self.state),
            "param" => Some(self.params),
            "source" => Some(self.source),
            "event" => Some(self.event),
            "item" => self.item,
            _ => None,
        }
    }
}

/// Why a template reference did not resolve. Never returned by [`Template::parse`]
/// — only by [`Template::resolve`]/[`resolve_value`], once a `Context` is in hand.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TemplateError {
    #[error("template root '{root}' is not bound in '{{{{{path}}}}}' (the roots are state, param, source, event, and item inside an action with `each`)")]
    UnknownRoot { root: String, path: String },
    #[error("template path '{{{{{path}}}}}' did not resolve to a value")]
    MissingPath { path: String },
}

/// One `{{...}}` occurrence's dotted path, root included (`["state", "freq"]`).
type Path = Vec<String>;

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Literal(String),
    Ref(Path),
}

/// A parsed string: either plain text, exactly one reference, or a mix.
/// [`Template::parse`] picks the representation; [`Template::resolve`] evaluates
/// it against a [`Context`].
#[derive(Debug, Clone, PartialEq)]
pub enum Template {
    /// No `{{…}}` at all — resolves to itself as a JSON string.
    Literal(String),
    /// The whole string is exactly one reference — resolves to that path's raw
    /// JSON value, unstringified (`"{{state.bins}}"` with `state.bins == 20`
    /// resolves to the *number* `20`, not the string `"20"`).
    Single(Path),
    /// Anything else containing at least one reference — resolves to a string,
    /// each reference stringified in place.
    Mixed(Vec<Segment>),
}

impl Template {
    /// Parse `s`. Infallible: a `{{` with no matching `}}`, or a path with no
    /// dots after the root, is simply not recognized as a reference and is kept
    /// as literal text — see the module docs.
    pub fn parse(s: &str) -> Template {
        let segments = split_segments(s);
        match segments.as_slice() {
            [] => Template::Literal(String::new()),
            [Segment::Literal(text)] => Template::Literal(text.clone()),
            [Segment::Ref(path)] => Template::Single(path.clone()),
            _ => Template::Mixed(segments),
        }
    }

    /// Evaluate against `ctx`. See the variant docs for what each shape resolves
    /// to.
    pub fn resolve(&self, ctx: &Context) -> Result<Value, TemplateError> {
        match self {
            Template::Literal(text) => Ok(Value::String(text.clone())),
            Template::Single(path) => resolve_path(path, ctx),
            Template::Mixed(segments) => {
                let mut out = String::new();
                for seg in segments {
                    match seg {
                        Segment::Literal(text) => out.push_str(text),
                        Segment::Ref(path) => {
                            let value = resolve_path(path, ctx)?;
                            out.push_str(&stringify(&value));
                        }
                    }
                }
                Ok(Value::String(out))
            }
        }
    }
}

/// Stringify a resolved value for splicing into mixed text: strings raw, numbers
/// and bools via `Display`, everything else (arrays, objects, null) as compact
/// JSON — matching the plan's "mixed text stringifies each reference" verbatim.
fn stringify(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn resolve_path(path: &Path, ctx: &Context) -> Result<Value, TemplateError> {
    let full = path.join(".");
    let (root_name, rest) = path
        .split_first()
        .ok_or_else(|| TemplateError::MissingPath { path: full.clone() })?;
    let mut cur = ctx
        .root(root_name)
        .ok_or_else(|| TemplateError::UnknownRoot {
            root: root_name.clone(),
            path: full.clone(),
        })?;
    for segment in rest {
        cur = match cur {
            Value::Object(map) => map
                .get(segment)
                .ok_or_else(|| TemplateError::MissingPath { path: full.clone() })?,
            // A numeric segment against an array indexes it, JSON-Pointer
            // style (`{{state.selected.0}}` is a path, not an operator —
            // see the module docs); out of range is `MissingPath` like any
            // other missing path. Against anything else — including an
            // array with a non-numeric segment — there is nowhere to go.
            Value::Array(items) => segment
                .parse::<usize>()
                .ok()
                .and_then(|i| items.get(i))
                .ok_or_else(|| TemplateError::MissingPath { path: full.clone() })?,
            _ => return Err(TemplateError::MissingPath { path: full.clone() }),
        };
    }
    Ok(cur.clone())
}

/// Resolve every template string anywhere inside `value`, recursing through
/// objects and arrays. Used for source `args`, and for every widget field that
/// carries opaque JSON (`table.rows`, `plot.spec`, action payloads, …).
pub fn resolve_value(value: &Value, ctx: &Context) -> Result<Value, TemplateError> {
    match value {
        Value::String(s) => Template::parse(s).resolve(ctx),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(resolve_value(item, ctx)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), resolve_value(v, ctx)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

/// Every `source.<name>` reference anywhere inside `value` (its immediate second
/// path segment), for [`crate::validate::validate`]'s cycle check and
/// [`crate::plan::plan`]'s dependency order. Walks strings the same way
/// [`resolve_value`] does, without needing a [`Context`] — parsing never fails.
pub fn source_refs_in(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_source_refs(value, &mut out);
    out
}

fn collect_source_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            for seg in template_segments(s) {
                if let Segment::Ref(path) = seg {
                    if path.first().map(String::as_str) == Some("source") {
                        if let Some(name) = path.get(1) {
                            if !out.contains(name) {
                                out.push(name.clone());
                            }
                        }
                    }
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_source_refs(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_source_refs(v, out)),
        _ => {}
    }
}

fn template_segments(s: &str) -> Vec<Segment> {
    split_segments(s)
}

/// The template roots, in the order the module docs list them.
pub const ROOTS: &[&str] = &["state", "param", "source", "event", "item"];

/// Whether `segment` is a plain path segment: `[A-Za-z0-9_-]+`.
pub fn is_plain_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The path inside a `{{…}}`, when it is a reference (see the module docs).
fn reference_path(inner: &str) -> Option<Path> {
    let path: Path = inner.trim().split('.').map(str::to_string).collect();
    let root_known = path.first().is_some_and(|r| ROOTS.contains(&r.as_str()));
    (root_known && path.iter().all(|s| is_plain_segment(s))).then_some(path)
}

/// Every `{{…}}` in `s` that was kept as literal text although it is shaped
/// like a dotted path (`{{stat.bins}}`) — a likely typo `validate` warns
/// about. A single word (`{{a}}`, LaTeX's `\frac{{a}}{b}`) is not listed.
pub fn literal_path_like(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = s[i..].find("{{") {
        let start = i + off;
        let Some(end) = find_close(s, start + 2) else {
            break;
        };
        let inner = s[start + 2..end].trim();
        let segments: Vec<&str> = inner.split('.').collect();
        if reference_path(inner).is_none()
            && segments.len() >= 2
            && segments.iter().all(|seg| is_plain_segment(seg))
        {
            out.push(inner.to_string());
        }
        i = end + 2;
    }
    out
}

/// The `{{...}}` scanner both `Template::parse` and the ref-collectors share.
/// A `{{…}}` that is not a reference (see the module docs), and a `{{` that
/// never finds a matching `}}`, are emitted as literal text, characters
/// included.
fn split_segments(s: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut literal = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = find_close(s, i + 2) {
                if let Some(path) = reference_path(&s[i + 2..end]) {
                    if !literal.is_empty() {
                        out.push(Segment::Literal(std::mem::take(&mut literal)));
                    }
                    out.push(Segment::Ref(path));
                    i = end + 2;
                    continue;
                }
            }
        }
        // Advance by one *character*, not one byte, so multi-byte UTF-8 text
        // outside a reference is never split mid-codepoint.
        let ch_len = s[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        literal.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    if !literal.is_empty() {
        out.push(Segment::Literal(literal));
    }
    out
}

/// The byte offset of the `}}` that closes a reference opened at `from` (just
/// past the `{{`), or `None` if the string ends first.
fn find_close(s: &str, from: usize) -> Option<usize> {
    s[from..].find("}}").map(|off| from + off)
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Template::Literal(s) => write!(f, "{s}"),
            Template::Single(p) => write!(f, "{{{{{}}}}}", p.join(".")),
            Template::Mixed(segs) => {
                for seg in segs {
                    match seg {
                        Segment::Literal(s) => write!(f, "{s}")?,
                        Segment::Ref(p) => write!(f, "{{{{{}}}}}", p.join("."))?,
                    }
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_string_is_literal() {
        let t = Template::parse("hello world");
        assert!(matches!(t, Template::Literal(_)));
        let state = Value::Null;
        let ctx = Context::new(&state, &state, &state, &state);
        assert_eq!(
            t.resolve(&ctx).unwrap(),
            Value::String("hello world".into())
        );
    }

    #[test]
    fn a_single_reference_resolves_unstringified() {
        let state = serde_json::json!({"bins": 20});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        let t = Template::parse("{{state.bins}}");
        assert_eq!(t.resolve(&ctx).unwrap(), serde_json::json!(20));
    }

    #[test]
    fn mixed_text_stringifies_numbers_and_bools() {
        let state = serde_json::json!({"bins": 20, "on": true});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        let t = Template::parse("bins={{state.bins}} on={{state.on}}");
        assert_eq!(
            t.resolve(&ctx).unwrap(),
            Value::String("bins=20 on=true".into())
        );
    }

    #[test]
    fn an_unknown_root_is_literal_text_not_a_reference() {
        let null = Value::Null;
        let ctx = Context::new(&null, &null, &null, &null);
        let t = Template::parse("{{nope.x}}");
        assert_eq!(t, Template::Literal("{{nope.x}}".into()));
        assert_eq!(t.resolve(&ctx).unwrap(), Value::String("{{nope.x}}".into()));
    }

    /// Review RS-S10: a formula in a text node is text.
    #[test]
    fn latex_and_typst_braces_are_kept_literal() {
        let state = serde_json::json!({"n": 3});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        for text in [
            r"$\frac{{a}}{b}$",
            "{{ }}",
            "{{a b}}",
            "set {{x: 1}}",
            "{{state.n + 1}}",
        ] {
            assert_eq!(
                Template::parse(text).resolve(&ctx).unwrap(),
                Value::String(text.to_string()),
                "{text}"
            );
        }
        assert_eq!(
            Template::parse(r"$x^{{state.n}}$").resolve(&ctx).unwrap(),
            Value::String(r"$x^3$".to_string())
        );
    }

    #[test]
    fn a_dotted_non_reference_is_reported_as_a_likely_typo() {
        assert_eq!(
            literal_path_like("{{stat.bins}} {{a}} {{state.x}}"),
            vec!["stat.bins"]
        );
    }

    #[test]
    fn missing_path_is_a_resolve_time_error() {
        let state = serde_json::json!({"bins": 20});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        let t = Template::parse("{{state.freq}}");
        assert_eq!(
            t.resolve(&ctx),
            Err(TemplateError::MissingPath {
                path: "state.freq".into()
            })
        );
    }

    #[test]
    fn resolve_value_walks_objects_and_arrays() {
        let state = serde_json::json!({"freq": 2.0});
        let source = serde_json::json!({"series": {"values": [1, 2, 3]}});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &source, &null);
        let args = serde_json::json!({
            "freq": "{{state.freq}}",
            "values": "{{source.series.values}}",
            "label": "freq={{state.freq}}",
        });
        let resolved = resolve_value(&args, &ctx).unwrap();
        assert_eq!(
            resolved,
            serde_json::json!({
                "freq": 2.0,
                "values": [1, 2, 3],
                "label": "freq=2.0",
            })
        );
    }

    #[test]
    fn source_refs_in_finds_every_dependency_once() {
        let args = serde_json::json!({
            "values": "{{source.series.values}}",
            "again": "{{source.series.count}}",
            "bins": "{{state.bins}}",
            "other": "{{source.hist.plot}}",
        });
        let mut refs = source_refs_in(&args);
        refs.sort();
        assert_eq!(refs, vec!["hist".to_string(), "series".to_string()]);
    }

    #[test]
    fn an_unclosed_reference_is_kept_as_literal_text() {
        let t = Template::parse("oops {{state.x");
        assert_eq!(t, Template::Literal("oops {{state.x".to_string()));
    }

    #[test]
    fn a_numeric_segment_indexes_an_array() {
        let state = serde_json::json!({"selected": ["a", "b", "c"]});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        let t = Template::parse("{{state.selected.1}}");
        assert_eq!(t.resolve(&ctx).unwrap(), serde_json::json!("b"));
    }

    #[test]
    fn an_out_of_range_numeric_segment_is_a_missing_path() {
        let state = serde_json::json!({"selected": ["a"]});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        let t = Template::parse("{{state.selected.5}}");
        assert_eq!(
            t.resolve(&ctx),
            Err(TemplateError::MissingPath {
                path: "state.selected.5".into()
            })
        );
    }

    #[test]
    fn a_numeric_segment_on_an_object_is_a_key_lookup() {
        let state = serde_json::json!({"row": {"0": "zeroth"}});
        let null = Value::Null;
        let ctx = Context::new(&state, &null, &null, &null);
        let t = Template::parse("{{state.row.0}}");
        assert_eq!(t.resolve(&ctx).unwrap(), serde_json::json!("zeroth"));
    }

    #[test]
    fn item_resolves_only_when_bound() {
        let null = Value::Null;
        let unbound = Context::new(&null, &null, &null, &null);
        let t = Template::parse("{{item}}");
        assert_eq!(
            t.resolve(&unbound),
            Err(TemplateError::UnknownRoot {
                root: "item".into(),
                path: "item".into()
            })
        );

        let element = serde_json::json!({"id": "paper-1"});
        let bound = unbound.with_item(&element);
        assert_eq!(t.resolve(&bound).unwrap(), element);
        let t_field = Template::parse("{{item.id}}");
        assert_eq!(
            t_field.resolve(&bound).unwrap(),
            serde_json::json!("paper-1")
        );
    }
}
