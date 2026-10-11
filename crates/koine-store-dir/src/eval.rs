//! Query evaluation: `Predicate`s, sorts and paging over rows held in memory.
//!
//! The directory store has no index, so a query lists the folder, parses the
//! files and runs them through this module. It is a transcription of what
//! `SqliteItemStore` does by compiling a query to SQL (`sql_query.rs` in
//! impress-core), because the SQLite store is the oracle of the conformance
//! suite (`docs/plan-koine-store.md` D2, D6):
//!
//! - **Three-valued logic.** A comparison against a NULL (a payload key the
//!   row does not have, an unset flag colour) is neither true nor false, so
//!   `Neq("payload.x", 1)` does not match a row without `x`, and
//!   `Not(Eq("payload.x", 1))` does not either. Only rows whose predicate is
//!   *true* are kept.
//! - **Column or payload.** `created`, `modified`, `author`, `is_read`, ...
//!   name the item's own columns; `payload.<path>` and any bare name that is
//!   not a column read the payload (`json_extract`).
//! - **Type order.** NULL, then numbers, then text; text compares bytewise.
//!   `true`/`false` read from the payload are `1`/`0`.
//! - **`Contains`** is a case-insensitive (ASCII) substring match, except on
//!   the five full-text fields (`title`, `author_text`, `abstract_text`,
//!   `note`, `body_content`), where SQLite answers from its FTS index: a
//!   phrase of whole tokens whose last token may be a prefix, searched across
//!   *all five* indexed columns of the row.
//! - **`offset` without `limit`** is ignored, as it is in the SQL.
//!
//! This module is pure (no I/O) so it can be lifted into a shared crate when
//! the memory store's evaluator and this one are deduplicated.

use std::cmp::Ordering;
use std::collections::HashMap;

use impress_item::{
    ActorKind, EdgeType, Item, ItemId, ItemQuery, Predicate, SortDescriptor, TypedReference, Value,
};

/// One row as a query sees it: the item, plus the operation target when the
/// row is an operation (the `op_target_id` column, NULL for every other row).
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    pub item: &'a Item,
    pub op_target: Option<ItemId>,
}

/// What a predicate needs from outside the row: the references of the items
/// named by `ReferencedBy` predicates (`source -> its references`).
#[derive(Debug, Default, Clone)]
pub struct Context {
    pub sources: HashMap<ItemId, Vec<TypedReference>>,
}

/// The ids whose references [`Context`] must hold to evaluate `q`.
pub fn needed_sources(q: &ItemQuery) -> Vec<ItemId> {
    fn walk(p: &Predicate, out: &mut Vec<ItemId>) {
        match p {
            Predicate::ReferencedBy(_, source) => out.push(*source),
            Predicate::And(ps) | Predicate::Or(ps) => ps.iter().for_each(|p| walk(p, out)),
            Predicate::Not(p) => walk(p, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    q.predicates.iter().for_each(|p| walk(p, &mut out));
    out.sort();
    out.dedup();
    out
}

/// A SQLite value: the four storage classes a column or `json_extract` yields.
#[derive(Debug, Clone, PartialEq)]
pub enum Sql {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl Sql {
    fn rank(&self) -> u8 {
        match self {
            Sql::Null => 0,
            Sql::Int(_) | Sql::Real(_) => 1,
            Sql::Text(_) => 2,
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            Sql::Int(i) => *i as f64,
            Sql::Real(r) => *r,
            _ => 0.0,
        }
    }

    /// `LIKE` and `Contains` read any non-NULL value as text.
    fn as_text(&self) -> Option<String> {
        match self {
            Sql::Null => None,
            Sql::Int(i) => Some(i.to_string()),
            Sql::Real(r) => Some(r.to_string()),
            Sql::Text(s) => Some(s.clone()),
        }
    }
}

/// The right-hand side of a comparison, as SQLite binds it
/// (`value_to_sql`): booleans are `1`/`0`, containers are their JSON text.
pub fn value_to_sql(v: &Value) -> Sql {
    match v {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Int(i64::from(*b)),
        Value::Int(i) => Sql::Int(*i),
        Value::Float(f) => Sql::Real(*f),
        Value::String(s) => Sql::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => {
            Sql::Text(serde_json::to_string(v).unwrap_or_default())
        }
    }
}

/// SQL comparison: `None` is "unknown" (a NULL on either side).
fn compare(a: &Sql, b: &Sql) -> Option<Ordering> {
    if matches!(a, Sql::Null) || matches!(b, Sql::Null) {
        return None;
    }
    Some(total_cmp(a, b))
}

/// The order `ORDER BY` uses: NULL < numbers < text.
fn total_cmp(a: &Sql, b: &Sql) -> Ordering {
    match (a, b) {
        (Sql::Int(x), Sql::Int(y)) => x.cmp(y),
        (Sql::Text(x), Sql::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ if a.rank() != b.rank() => a.rank().cmp(&b.rank()),
        (Sql::Null, Sql::Null) => Ordering::Equal,
        _ => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(Ordering::Equal),
    }
}

fn actor_kind_str(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

fn opt_text(s: &Option<String>) -> Sql {
    s.as_ref().map_or(Sql::Null, |s| Sql::Text(s.clone()))
}

fn opt_id(id: Option<ItemId>) -> Sql {
    id.map_or(Sql::Null, |id| Sql::Text(id.to_string()))
}

/// `field_to_column` for a predicate.
fn column(field: &str, row: &Row<'_>) -> Sql {
    let item = row.item;
    match field {
        "id" => Sql::Text(item.id.to_string()),
        "schema" | "schema_ref" => Sql::Text(item.schema.to_string()),
        "created" => Sql::Int(item.created.timestamp_millis()),
        "modified" => Sql::Int(item.modified.timestamp_millis()),
        "author" => Sql::Text(item.author.clone()),
        "author_kind" => Sql::Text(actor_kind_str(item.author_kind).into()),
        "is_read" => Sql::Int(i64::from(item.is_read)),
        "is_starred" => Sql::Int(i64::from(item.is_starred)),
        "flag_color" => item
            .flag
            .as_ref()
            .map_or(Sql::Null, |f| Sql::Text(f.color.clone())),
        "flag_style" => item.flag.as_ref().map_or(Sql::Null, |f| opt_text(&f.style)),
        "flag_length" => item
            .flag
            .as_ref()
            .map_or(Sql::Null, |f| opt_text(&f.length)),
        "parent_id" | "parent" => opt_id(item.parent),
        "logical_clock" => Sql::Int(item.logical_clock as i64),
        "origin" => opt_text(&item.origin),
        "canonical_id" => opt_text(&item.canonical_id),
        "priority" => Sql::Text(item.priority.to_string()),
        "visibility" => Sql::Text(item.visibility.to_string()),
        "message_type" => opt_text(&item.message_type),
        "produced_by" => opt_id(item.produced_by),
        "version" => opt_text(&item.version),
        "batch_id" => opt_text(&item.batch_id),
        "op_target_id" => opt_id(row.op_target),
        f => payload_column(&item.payload, f.strip_prefix("payload.").unwrap_or(f)),
    }
}

/// `sort_field_to_column`: fewer columns than a predicate may name; anything
/// else is a payload path (so `author` sorts by `payload.author`, not by the
/// column — the SQL does the same).
fn sort_column(field: &str, row: &Row<'_>) -> Sql {
    match field {
        "created" | "modified" | "is_read" | "is_starred" | "flag_color" | "id"
        | "logical_clock" => column(field, row),
        f => payload_column(&row.item.payload, f.strip_prefix("payload.").unwrap_or(f)),
    }
}

/// `json_extract(payload, '$.<path>')`. A path with a character outside
/// `[alnum . _ - [ ]]` is NULL (the SQL refuses to splice it); `.` descends
/// into objects and `[n]` indexes arrays.
fn payload_column(payload: &std::collections::BTreeMap<String, Value>, path: &str) -> Sql {
    let safe = path
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '[' | ']'));
    if !safe || path.is_empty() {
        return Sql::Null;
    }
    let mut segments = path.split('.');
    let Some(first) = segments.next() else {
        return Sql::Null;
    };
    let mut current: Option<&Value> = None;
    let mut first_segment = true;
    for segment in std::iter::once(first).chain(segments) {
        let (key, subscripts) = match segment.find('[') {
            Some(at) => (&segment[..at], &segment[at..]),
            None => (segment, ""),
        };
        if !key.is_empty() {
            let next = if first_segment {
                payload.get(key)
            } else {
                match current {
                    Some(Value::Object(m)) => m.get(key),
                    _ => None,
                }
            };
            match next {
                Some(v) => current = Some(v),
                None => return Sql::Null,
            }
        } else if first_segment {
            return Sql::Null;
        }
        first_segment = false;
        let mut rest = subscripts;
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else {
                return Sql::Null;
            };
            let Ok(index) = stripped[..close].parse::<usize>() else {
                return Sql::Null;
            };
            match current {
                Some(Value::Array(a)) => match a.get(index) {
                    Some(v) => current = Some(v),
                    None => return Sql::Null,
                },
                _ => return Sql::Null,
            }
            rest = &stripped[close + 1..];
        }
        if !rest.is_empty() {
            return Sql::Null;
        }
    }
    match current {
        Some(v) => value_to_sql(v),
        None => Sql::Null,
    }
}

/// Is `row` kept by `q`: the schema (exact equality) and every predicate true.
pub fn matches(q: &ItemQuery, row: &Row<'_>, ctx: &Context) -> bool {
    if let Some(schema) = &q.schema {
        if row.item.schema.as_str() != schema.as_str() {
            return false;
        }
    }
    q.predicates
        .iter()
        .all(|p| predicate(p, row, ctx) == Some(true))
}

fn and3(values: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for v in values {
        match v {
            Some(false) => return Some(false),
            None => unknown = true,
            Some(true) => {}
        }
    }
    if unknown {
        None
    } else {
        Some(true)
    }
}

fn or3(values: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for v in values {
        match v {
            Some(true) => return Some(true),
            None => unknown = true,
            Some(false) => {}
        }
    }
    if unknown {
        None
    } else {
        Some(false)
    }
}

/// Evaluate one predicate; `None` is SQL's "unknown".
pub fn predicate(p: &Predicate, row: &Row<'_>, ctx: &Context) -> Option<bool> {
    let cmp = |field: &str, value: &Value, test: fn(Ordering) -> bool| {
        compare(&column(field, row), &value_to_sql(value)).map(test)
    };
    match p {
        Predicate::Eq(f, v) => cmp(f, v, |o| o == Ordering::Equal),
        Predicate::Neq(f, v) => cmp(f, v, |o| o != Ordering::Equal),
        Predicate::Gt(f, v) => cmp(f, v, |o| o == Ordering::Greater),
        Predicate::Lt(f, v) => cmp(f, v, |o| o == Ordering::Less),
        Predicate::Gte(f, v) => cmp(f, v, |o| o != Ordering::Less),
        Predicate::Lte(f, v) => cmp(f, v, |o| o != Ordering::Greater),
        Predicate::Contains(field, text) => contains(field, text, row),
        // `x IN ()` is FALSE for every row, NULL columns included.
        Predicate::In(_, values) if values.is_empty() => Some(false),
        Predicate::In(field, values) => {
            let col = column(field, row);
            if matches!(col, Sql::Null) {
                return None;
            }
            let mut saw_null = false;
            for v in values {
                match compare(&col, &value_to_sql(v)) {
                    Some(Ordering::Equal) => return Some(true),
                    Some(_) => {}
                    None => saw_null = true,
                }
            }
            if saw_null {
                None
            } else {
                Some(false)
            }
        }
        // The tag itself or any descendant (`tag/...`).
        Predicate::HasTag(tag) => {
            let prefix = format!("{}/", tag.to_ascii_lowercase());
            Some(
                row.item
                    .tags
                    .iter()
                    .any(|t| t == tag || t.to_ascii_lowercase().starts_with(&prefix)),
            )
        }
        Predicate::HasFlag(Some(color)) => {
            Some(row.item.flag.as_ref().is_some_and(|f| f.color == *color))
        }
        Predicate::HasFlag(None) => Some(row.item.flag.is_some()),
        Predicate::IsRead(v) => Some(row.item.is_read == *v),
        Predicate::IsStarred(v) => Some(row.item.is_starred == *v),
        Predicate::HasParent(id) => Some(row.item.parent == Some(*id)),
        Predicate::HasReference(edge, target) => Some(
            row.item
                .references
                .iter()
                .any(|r| r.target == *target && r.edge_type == *edge),
        ),
        Predicate::ReferencedBy(edge, source) => {
            Some(ctx.sources.get(source).is_some_and(|refs| {
                refs.iter()
                    .any(|r| r.target == row.item.id && r.edge_type == *edge)
            }))
        }
        Predicate::And(ps) => and3(ps.iter().map(|p| predicate(p, row, ctx))),
        Predicate::Or(ps) => or3(ps.iter().map(|p| predicate(p, row, ctx))),
        Predicate::Not(p) => predicate(p, row, ctx).map(|b| !b),
    }
}

fn is_fts_field(field: &str) -> bool {
    matches!(
        field,
        "title"
            | "author_text"
            | "abstract_text"
            | "note"
            | "body_content"
            | "payload.title"
            | "payload.author_text"
            | "payload.abstract_text"
            | "payload.note"
            | "payload.body_content"
    )
}

fn contains(field: &str, text: &str, row: &Row<'_>) -> Option<bool> {
    if is_fts_field(field) {
        return Some(fts_match(row.item, text));
    }
    let haystack = column(field, row).as_text()?;
    Some(
        haystack
            .to_ascii_lowercase()
            .contains(&text.to_ascii_lowercase()),
    )
}

/// The five FTS columns of a row (`update_fts`): title (or subject), author
/// text (or from), abstract, note, body (`body_content` or `body`).
fn fts_columns(item: &Item) -> [Option<String>; 5] {
    let get = |k: &str| match item.payload.get(k) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    [
        get("title").or_else(|| get("subject")),
        get("author_text").or_else(|| get("from")),
        get("abstract_text"),
        get("note"),
        get("body_content").or_else(|| get("body")),
    ]
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `"<phrase>"*` against the indexed columns: the phrase's tokens appear
/// consecutively in one column, the last as a prefix.
fn fts_match(item: &Item, text: &str) -> bool {
    let wanted = tokens(text);
    let Some((last, head)) = wanted.split_last() else {
        return false;
    };
    fts_columns(item).iter().flatten().any(|column| {
        let have = tokens(column);
        have.len() >= wanted.len()
            && have
                .windows(wanted.len())
                .any(|w| w[..head.len()] == *head && w[head.len()].starts_with(last.as_str()))
    })
}

/// Sort `rows` by `sorts` (stable; no sort keeps the caller's order).
pub fn sort_rows<T>(rows: &mut [T], sorts: &[SortDescriptor], row_of: impl Fn(&T) -> Row<'_>) {
    if sorts.is_empty() {
        return;
    }
    rows.sort_by(|a, b| {
        let (ra, rb) = (row_of(a), row_of(b));
        for s in sorts {
            let o = total_cmp(&sort_column(&s.field, &ra), &sort_column(&s.field, &rb));
            let o = if s.ascending { o } else { o.reverse() };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
}

/// `LIMIT n OFFSET m`: the offset only applies together with a limit.
pub fn page<T>(rows: Vec<T>, q: &ItemQuery) -> Vec<T> {
    match q.limit {
        Some(limit) => rows
            .into_iter()
            .skip(q.offset.unwrap_or(0))
            .take(limit)
            .collect(),
        None => rows,
    }
}

/// Does `edges` (empty = every type) admit `edge`.
pub fn edge_admitted(edges: &[EdgeType], edge: &EdgeType) -> bool {
    edges.is_empty() || edges.contains(edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use impress_item::{FlagState, Priority, SchemaRef, Visibility};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn item(payload: &[(&str, Value)]) -> Item {
        Item {
            id: Uuid::new_v4(),
            schema: SchemaRef::from_stored("test".into()),
            payload: payload
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
            created: Utc::now(),
            modified: Utc::now(),
            author: "a".into(),
            author_kind: ActorKind::Human,
            logical_clock: 7,
            origin: None,
            canonical_id: None,
            tags: vec!["methods/sims".into()],
            flag: Some(FlagState {
                color: "red".into(),
                style: None,
                length: None,
            }),
            is_read: false,
            is_starred: true,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
        }
    }

    fn eval(item: &Item, p: Predicate) -> Option<bool> {
        predicate(
            &p,
            &Row {
                item,
                op_target: None,
            },
            &Context::default(),
        )
    }

    #[test]
    fn a_missing_payload_key_is_unknown_not_false() {
        let i = item(&[("n", Value::Int(3))]);
        let eq = |k: &str, v: i64| Predicate::Eq(k.into(), Value::Int(v));
        assert_eq!(eval(&i, eq("payload.n", 3)), Some(true));
        assert_eq!(eval(&i, eq("payload.absent", 3)), None);
        assert_eq!(
            eval(&i, Predicate::Neq("payload.absent".into(), Value::Int(3))),
            None
        );
        assert_eq!(
            eval(&i, Predicate::Not(Box::new(eq("payload.absent", 3)))),
            None
        );
        assert_eq!(eval(&i, Predicate::Or(vec![])), Some(false));
        assert_eq!(eval(&i, Predicate::And(vec![])), Some(true));
    }

    #[test]
    fn numbers_sort_before_text_and_bools_are_ints() {
        let i = item(&[("b", Value::Bool(true)), ("s", Value::String("x".into()))]);
        assert_eq!(
            eval(&i, Predicate::Eq("payload.b".into(), Value::Int(1))),
            Some(true)
        );
        assert_eq!(
            eval(
                &i,
                Predicate::Lt("payload.b".into(), Value::String("a".into()))
            ),
            Some(true)
        );
        assert_eq!(
            eval(&i, Predicate::Gt("payload.s".into(), Value::Int(99))),
            Some(true)
        );
    }

    #[test]
    fn contains_is_a_case_insensitive_substring_except_on_fts_fields() {
        let i = item(&[
            ("title", Value::String("Dark Matter in Galaxies".into())),
            ("doi", Value::String("10.1234/ABC".into())),
        ]);
        let c = |f: &str, t: &str| eval(&i, Predicate::Contains(f.into(), t.into()));
        assert_eq!(c("payload.doi", "1234/abc"), Some(true));
        assert_eq!(c("payload.missing", "x"), None);
        // FTS: whole tokens, last one a prefix.
        assert_eq!(c("title", "dark mat"), Some(true));
        assert_eq!(c("title", "ark matter"), Some(false));
        assert_eq!(c("title", "matter dark"), Some(false));
    }

    #[test]
    fn has_tag_matches_descendants_and_in_handles_empty_and_null() {
        let i = item(&[]);
        assert_eq!(eval(&i, Predicate::HasTag("methods".into())), Some(true));
        assert_eq!(eval(&i, Predicate::HasTag("meth".into())), Some(false));
        assert_eq!(eval(&i, Predicate::In("id".into(), vec![])), Some(false));
        assert_eq!(
            eval(&i, Predicate::In("parent".into(), vec![Value::Int(1)])),
            None
        );
    }

    #[test]
    fn payload_paths_descend_and_unsafe_paths_are_null() {
        let mut inner = BTreeMap::new();
        inner.insert("k".to_string(), Value::Int(9));
        let i = item(&[
            ("o", Value::Object(inner)),
            ("a", Value::Array(vec![Value::Int(4), Value::Int(5)])),
        ]);
        let row = Row {
            item: &i,
            op_target: None,
        };
        assert_eq!(column("payload.o.k", &row), Sql::Int(9));
        assert_eq!(column("payload.a[1]", &row), Sql::Int(5));
        assert_eq!(column("payload.a[9]", &row), Sql::Null);
        assert_eq!(column("payload.o'; drop", &row), Sql::Null);
    }

    #[test]
    fn offset_without_limit_is_ignored() {
        let q = ItemQuery {
            offset: Some(2),
            ..Default::default()
        };
        assert_eq!(page(vec![1, 2, 3], &q), vec![1, 2, 3]);
        let q = ItemQuery {
            offset: Some(1),
            limit: Some(1),
            ..Default::default()
        };
        assert_eq!(page(vec![1, 2, 3], &q), vec![2]);
    }
}
