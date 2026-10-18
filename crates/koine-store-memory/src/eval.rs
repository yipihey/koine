//! Query evaluation, written once.
//!
//! This module is public so that `koine-store-dir` (a scan-based backend)
//! depends on this crate for exactly this and nothing else: a second copy of
//! predicate evaluation is a second definition of what a query means, and the
//! two drift. Everything here is pure: an item in, a verdict out.
//!
//! The oracle is `SqliteItemStore`'s query compiler (`sql_query.rs` in
//! impress-apps), whose SQL this module reproduces without SQLite:
//!
//! * **Three-valued logic.** A comparison against a missing field is SQL
//!   `NULL`, not false, and `NOT NULL` is `NULL`. So `Neq("payload.x", v)`
//!   and `Not(Eq("payload.x", v))` both *exclude* an item that has no `x`,
//!   `Not(HasFlag(Some("red")))` excludes every unflagged item, and
//!   `Not(HasParent(p))` excludes every parentless one. A row is returned
//!   only when the whole `WHERE` is true. [`eval`] returns `Option<bool>`
//!   (`None` is `NULL`); [`matches`] is "is `Some(true)`".
//! * **Fields.** The item's own columns are addressed by name (`id`,
//!   `schema`/`schema_ref`, `created`, `modified` as epoch milliseconds,
//!   `author`, `author_kind` as `human|agent|system`, `is_read`,
//!   `is_starred` as 0/1, `flag_color|flag_style|flag_length`,
//!   `parent`/`parent_id`, `logical_clock`, `origin`, `canonical_id`,
//!   `priority`, `visibility`, `message_type`, `produced_by`, `version`,
//!   `batch_id`, `op_target_id`). Anything else is a payload path:
//!   `payload.a.b[0]` and the bare `a.b[0]` are the same thing, a JSON path
//!   into the payload. A path with a character outside
//!   `[alphanumeric . _ - $ [ ]]` is `NULL`.
//! * **Values.** A payload value compares as SQLite's `json_extract` returns
//!   it: `null` is `NULL`, `true`/`false` are the integers 1/0, numbers are
//!   integers or reals, strings are text, arrays and objects are their
//!   compact JSON text. Integers and reals compare numerically with each
//!   other; any number sorts before any text; text compares bytewise.
//! * **`Contains`.** On `title`, `author_text`, `abstract_text`, `note` and
//!   `body_content` (bare or `payload.`-prefixed) it is the full-text index:
//!   a case-insensitive phrase whose last word is a prefix ("dark mat"
//!   finds "Dark Matter"), matched against *any* of the five indexed
//!   columns (`title`/`subject`, `author_text`/`from`, `abstract_text`,
//!   `note`, `body_content`/`body`) because the oracle's `MATCH` is not
//!   column-restricted. On every other field it is a case-insensitive
//!   (ASCII) substring, SQLite `LIKE`.
//! * **`HasTag(t)`** is the tag `t` or any tag below it: `tag = t` or
//!   `tag LIKE 't/%'` (so the prefix test is ASCII-case-insensitive, and a
//!   `%` or `_` in `t` is a wildcard, exactly as the oracle's SQL has it).
//! * **Sorting** orders `NULL` first ascending (last descending), then
//!   numbers, then text. The sortable names are `created`, `modified`,
//!   `is_read`, `is_starred`, `flag_color`, `id`, `logical_clock`; any other
//!   name is a payload path, **including `priority` and `author`**, which are
//!   not payload fields and so sort as `NULL` (the oracle's rule).
//! * **Paging.** `offset` applies only together with `limit`; an `offset`
//!   with no `limit` is ignored. `count` ignores both.
//!
//! Not reproduced: SQLite's column affinity (an integer parameter compared to
//! a text column is not converted to text, nor numeric-looking text to a
//! number on an integer column), and a malformed JSON path, which SQLite
//! rejects with an error and this module evaluates as `NULL`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use impress_item::{
    ActorKind, EdgeType, Item, ItemId, ItemQuery, Predicate, SortDescriptor, TypedReference, Value,
};

/// A SQLite-style scalar: what a field is when it is compared or sorted.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl Scalar {
    /// A query parameter or payload value as the oracle binds it.
    pub fn from_value(value: &Value) -> Scalar {
        match value {
            Value::Null => Scalar::Null,
            Value::Bool(b) => Scalar::Int(i64::from(*b)),
            Value::Int(i) => Scalar::Int(*i),
            // SQLite binds NaN as NULL.
            Value::Float(f) if f.is_nan() => Scalar::Null,
            Value::Float(f) => Scalar::Real(*f),
            Value::String(s) => Scalar::Text(s.clone()),
            Value::Array(_) | Value::Object(_) => {
                Scalar::Text(serde_json::to_string(value).unwrap_or_default())
            }
        }
    }

    fn text(s: Option<&str>) -> Scalar {
        s.map_or(Scalar::Null, |s| Scalar::Text(s.to_owned()))
    }

    fn id(id: Option<ItemId>) -> Scalar {
        id.map_or(Scalar::Null, |id| Scalar::Text(id.to_string()))
    }

    /// The text `LIKE` sees: numbers are converted, `NULL` stays `NULL`.
    fn as_like_text(&self) -> Option<String> {
        match self {
            Scalar::Null => None,
            Scalar::Int(i) => Some(i.to_string()),
            Scalar::Real(f) if f.is_finite() && f.fract() == 0.0 => Some(format!("{f:.1}")),
            Scalar::Real(f) => Some(f.to_string()),
            Scalar::Text(s) => Some(s.clone()),
        }
    }
}

/// SQLite's storage-class order, for sorting: `NULL` < numbers < text.
/// Integers and reals compare numerically.
pub fn total_cmp(a: &Scalar, b: &Scalar) -> Ordering {
    fn class(s: &Scalar) -> u8 {
        match s {
            Scalar::Null => 0,
            Scalar::Int(_) | Scalar::Real(_) => 1,
            Scalar::Text(_) => 2,
        }
    }
    match (a, b) {
        (Scalar::Int(x), Scalar::Int(y)) => x.cmp(y),
        (Scalar::Int(x), Scalar::Real(y)) => (*x as f64).total_cmp(y),
        (Scalar::Real(x), Scalar::Int(y)) => x.total_cmp(&(*y as f64)),
        (Scalar::Real(x), Scalar::Real(y)) => x.total_cmp(y),
        (Scalar::Text(x), Scalar::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => class(a).cmp(&class(b)),
    }
}

/// SQL comparison: `None` when either side is `NULL`.
pub fn sql_cmp(a: &Scalar, b: &Scalar) -> Option<Ordering> {
    if matches!(a, Scalar::Null) || matches!(b, Scalar::Null) {
        None
    } else {
        Some(total_cmp(a, b))
    }
}

/// An item as the evaluator sees it: the item, plus the one column an
/// [`Item`] does not carry. Operation rows are items with an `op_target_id`
/// (the item the operation was applied to); every other row has none.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    pub item: &'a Item,
    pub op_target: Option<ItemId>,
}

impl<'a> Row<'a> {
    /// An ordinary (non-operation) row.
    pub fn of(item: &'a Item) -> Self {
        Row {
            item,
            op_target: None,
        }
    }
}

/// The one question `ReferencedBy` asks beyond the row in hand.
pub trait Graph {
    /// Does `source` hold a reference to `target` along `edge`?
    fn has_edge(&self, source: ItemId, edge: &EdgeType, target: ItemId) -> bool;
}

impl<F> Graph for F
where
    F: Fn(ItemId, &EdgeType, ItemId) -> bool,
{
    fn has_edge(&self, source: ItemId, edge: &EdgeType, target: ItemId) -> bool {
        self(source, edge, target)
    }
}

/// A graph with no edges, for a caller that has no other rows to consult
/// (`ReferencedBy` is then false for everything).
pub struct NoGraph;

impl Graph for NoGraph {
    fn has_edge(&self, _: ItemId, _: &EdgeType, _: ItemId) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

fn author_kind_str(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

/// The value of `field` for a predicate (`field_to_column` in the oracle).
pub fn field_value(row: Row<'_>, field: &str) -> Scalar {
    let item = row.item;
    match field {
        "id" => Scalar::Text(item.id.to_string()),
        "schema" | "schema_ref" => Scalar::Text(item.schema.as_str().to_owned()),
        "created" => Scalar::Int(item.created.timestamp_millis()),
        "modified" => Scalar::Int(item.modified.timestamp_millis()),
        "author" => Scalar::Text(item.author.clone()),
        "author_kind" => Scalar::Text(author_kind_str(item.author_kind).to_owned()),
        "is_read" => Scalar::Int(i64::from(item.is_read)),
        "is_starred" => Scalar::Int(i64::from(item.is_starred)),
        "flag_color" => Scalar::text(item.flag.as_ref().map(|f| f.color.as_str())),
        "flag_style" => Scalar::text(item.flag.as_ref().and_then(|f| f.style.as_deref())),
        "flag_length" => Scalar::text(item.flag.as_ref().and_then(|f| f.length.as_deref())),
        "parent_id" | "parent" => Scalar::id(item.parent),
        "logical_clock" => Scalar::Int(item.logical_clock as i64),
        "origin" => Scalar::text(item.origin.as_deref()),
        "canonical_id" => Scalar::text(item.canonical_id.as_deref()),
        "priority" => Scalar::Text(item.priority.to_string()),
        "visibility" => Scalar::Text(item.visibility.to_string()),
        "message_type" => Scalar::text(item.message_type.as_deref()),
        "produced_by" => Scalar::id(item.produced_by),
        "version" => Scalar::text(item.version.as_deref()),
        "batch_id" => Scalar::text(item.batch_id.as_deref()),
        "op_target_id" => Scalar::id(row.op_target),
        other => payload_scalar(
            &item.payload,
            other.strip_prefix("payload.").unwrap_or(other),
        ),
    }
}

/// The value of `field` for a sort key (`sort_field_to_column` in the oracle):
/// a narrower set of columns than [`field_value`] accepts.
pub fn sort_value(item: &Item, field: &str) -> Scalar {
    match field {
        "created" => Scalar::Int(item.created.timestamp_millis()),
        "modified" => Scalar::Int(item.modified.timestamp_millis()),
        "is_read" => Scalar::Int(i64::from(item.is_read)),
        "is_starred" => Scalar::Int(i64::from(item.is_starred)),
        "flag_color" => Scalar::text(item.flag.as_ref().map(|f| f.color.as_str())),
        "id" => Scalar::Text(item.id.to_string()),
        "logical_clock" => Scalar::Int(item.logical_clock as i64),
        other => payload_scalar(
            &item.payload,
            other.strip_prefix("payload.").unwrap_or(other),
        ),
    }
}

enum Step<'a> {
    Key(&'a str),
    Index(usize),
}

/// Split `a.b[0].c` into steps; `None` for a path SQLite would reject.
fn parse_path(path: &str) -> Option<Vec<Step<'_>>> {
    let mut steps = Vec::new();
    let mut rest = path;
    loop {
        // A label runs to the next '.' or '['.
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let (label, tail) = rest.split_at(end);
        if label.is_empty() {
            return None;
        }
        steps.push(Step::Key(label));
        rest = tail;
        while let Some(after) = rest.strip_prefix('[') {
            let close = after.find(']')?;
            steps.push(Step::Index(after[..close].parse().ok()?));
            rest = &after[close + 1..];
        }
        if rest.is_empty() {
            return Some(steps);
        }
        rest = rest.strip_prefix('.')?;
    }
}

/// `json_extract(payload, '$.<path>')`.
fn payload_scalar(payload: &BTreeMap<String, Value>, path: &str) -> Scalar {
    let safe = path
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '$' | '[' | ']'));
    let Some(steps) = safe.then(|| parse_path(path)).flatten() else {
        return Scalar::Null;
    };
    let mut steps = steps.into_iter();
    let Some(Step::Key(first)) = steps.next() else {
        return Scalar::Null;
    };
    let mut current = payload.get(first);
    for step in steps {
        current = match (step, current) {
            (Step::Key(key), Some(Value::Object(map))) => map.get(key),
            (Step::Index(i), Some(Value::Array(items))) => items.get(i),
            _ => None,
        };
    }
    current.map_or(Scalar::Null, Scalar::from_value)
}

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// `NULL`-aware conjunction: false beats unknown beats true.
fn and3(parts: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for p in parts {
        match p {
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

/// `NULL`-aware disjunction: true beats unknown beats false.
fn or3(parts: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for p in parts {
        match p {
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

fn compare_with(
    row: Row<'_>,
    field: &str,
    value: &Value,
    accept: fn(Ordering) -> bool,
) -> Option<bool> {
    sql_cmp(&field_value(row, field), &Scalar::from_value(value)).map(accept)
}

/// Evaluate one predicate: `Some(true)`, `Some(false)`, or `None` for SQL
/// `NULL` (see the module doc).
pub fn eval(row: Row<'_>, predicate: &Predicate, graph: &dyn Graph) -> Option<bool> {
    let item = row.item;
    match predicate {
        Predicate::Eq(f, v) => compare_with(row, f, v, |o| o == Ordering::Equal),
        Predicate::Neq(f, v) => compare_with(row, f, v, |o| o != Ordering::Equal),
        Predicate::Gt(f, v) => compare_with(row, f, v, |o| o == Ordering::Greater),
        Predicate::Lt(f, v) => compare_with(row, f, v, |o| o == Ordering::Less),
        Predicate::Gte(f, v) => compare_with(row, f, v, |o| o != Ordering::Less),
        Predicate::Lte(f, v) => compare_with(row, f, v, |o| o != Ordering::Greater),
        Predicate::Contains(field, text) => contains(row, field, text),
        // The oracle compiles an empty set to the constant 0, not to a
        // comparison: it is false, never NULL.
        Predicate::In(_, values) if values.is_empty() => Some(false),
        Predicate::In(field, values) => {
            let operand = field_value(row, field);
            if operand == Scalar::Null {
                return None;
            }
            let mut saw_null = false;
            for v in values {
                match sql_cmp(&operand, &Scalar::from_value(v)) {
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
        Predicate::HasTag(tag) => {
            let below: Vec<char> = format!("{tag}/%").chars().collect();
            Some(
                item.tags
                    .iter()
                    .any(|t| t == tag || like(&below, &t.chars().collect::<Vec<_>>(), None)),
            )
        }
        Predicate::HasFlag(Some(color)) => item.flag.as_ref().map(|f| &f.color == color),
        Predicate::HasFlag(None) => Some(item.flag.is_some()),
        Predicate::IsRead(v) => Some(item.is_read == *v),
        Predicate::IsStarred(v) => Some(item.is_starred == *v),
        Predicate::HasParent(id) => item.parent.map(|p| p == *id),
        Predicate::HasReference(edge, target) => Some(
            item.references
                .iter()
                .any(|r| r.target == *target && &r.edge_type == edge),
        ),
        Predicate::ReferencedBy(edge, source) => Some(graph.has_edge(*source, edge, item.id)),
        Predicate::And(parts) => and3(parts.iter().map(|p| eval(row, p, graph))),
        Predicate::Or(parts) => or3(parts.iter().map(|p| eval(row, p, graph))),
        Predicate::Not(inner) => eval(row, inner, graph).map(|v| !v),
    }
}

/// Does the row satisfy the predicate? (`NULL` is no.)
pub fn matches(row: Row<'_>, predicate: &Predicate, graph: &dyn Graph) -> bool {
    eval(row, predicate, graph) == Some(true)
}

/// Does the row satisfy the query's schema (exact equality, `None` meaning
/// every kind) and every one of its predicates? Ignores sort and paging.
pub fn matches_query(row: Row<'_>, q: &ItemQuery, graph: &dyn Graph) -> bool {
    if let Some(schema) = &q.schema {
        if schema.as_str() != row.item.schema.as_str() {
            return false;
        }
    }
    q.predicates.iter().all(|p| matches(row, p, graph))
}

// ---------------------------------------------------------------------------
// Contains: full-text on five fields, LIKE on the rest
// ---------------------------------------------------------------------------

fn is_fts_field(field: &str) -> bool {
    matches!(
        field.strip_prefix("payload.").unwrap_or(field),
        "title" | "author_text" | "abstract_text" | "note" | "body_content"
    )
}

fn contains(row: Row<'_>, field: &str, text: &str) -> Option<bool> {
    if is_fts_field(field) {
        return Some(fts_match(&row.item.payload, text));
    }
    let operand = field_value(row, field).as_like_text()?;
    let escaped = text.replace('%', "\\%").replace('_', "\\_");
    let pattern: Vec<char> = format!("%{escaped}%").chars().collect();
    let operand: Vec<char> = operand.chars().collect();
    Some(like(&pattern, &operand, Some('\\')))
}

/// SQLite `LIKE`: `%` and `_` wildcards, ASCII-case-insensitive, with an
/// optional escape character.
fn like(pattern: &[char], text: &[char], escape: Option<char>) -> bool {
    let Some((&c, rest)) = pattern.split_first() else {
        return text.is_empty();
    };
    if c == '%' {
        let mut rest = rest;
        while let ['%', tail @ ..] = rest {
            rest = tail;
        }
        if rest.is_empty() {
            return true;
        }
        return (0..=text.len()).any(|i| like(rest, &text[i..], escape));
    }
    if Some(c) == escape {
        let Some((&literal, rest)) = rest.split_first() else {
            return false;
        };
        return text
            .split_first()
            .is_some_and(|(t, text)| t.eq_ignore_ascii_case(&literal) && like(rest, text, escape));
    }
    let Some((t, text)) = text.split_first() else {
        return false;
    };
    (c == '_' || t.eq_ignore_ascii_case(&c)) && like(rest, text, escape)
}

fn string_field<'a>(payload: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    match payload.get(key) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

/// The five indexed columns of the oracle's `items_fts`, filled as it fills
/// them: mail payloads fold `subject`/`from`/`body` into the canonical names.
fn fts_columns(payload: &BTreeMap<String, Value>) -> [&str; 5] {
    [
        string_field(payload, "title")
            .or_else(|| string_field(payload, "subject"))
            .unwrap_or(""),
        string_field(payload, "author_text")
            .or_else(|| string_field(payload, "from"))
            .unwrap_or(""),
        string_field(payload, "abstract_text").unwrap_or(""),
        string_field(payload, "note").unwrap_or(""),
        string_field(payload, "body_content")
            .or_else(|| string_field(payload, "body"))
            .unwrap_or(""),
    ]
}

/// The tokenizer: runs of alphanumeric characters, case-folded.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `items_fts MATCH '"<text>"*'`: the query words in a row, the last one a
/// prefix, inside any one indexed column.
fn fts_match(payload: &BTreeMap<String, Value>, text: &str) -> bool {
    let query = tokens(text);
    let Some((last, head)) = query.split_last() else {
        return false;
    };
    fts_columns(payload).iter().any(|column| {
        let words = tokens(column);
        words.len() >= query.len()
            && words
                .windows(query.len())
                .any(|w| w[..head.len()] == *head && w[head.len()].starts_with(last.as_str()))
    })
}

// ---------------------------------------------------------------------------
// Sorting and paging
// ---------------------------------------------------------------------------

/// Compare two items by a list of sort descriptors (the first that differs
/// decides).
pub fn compare_items(a: &Item, b: &Item, sorts: &[SortDescriptor]) -> Ordering {
    for sort in sorts {
        let ordering = total_cmp(&sort_value(a, &sort.field), &sort_value(b, &sort.field));
        let ordering = if sort.ascending {
            ordering
        } else {
            ordering.reverse()
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

/// Stable-sort `rows` by `sorts`; rows equal on every key keep the order they
/// came in (the oracle leaves that order unspecified).
pub fn sort_rows<T>(rows: &mut [T], sorts: &[SortDescriptor], item_of: impl Fn(&T) -> &Item) {
    if sorts.is_empty() {
        return;
    }
    rows.sort_by(|a, b| compare_items(item_of(a), item_of(b), sorts));
}

/// Apply `limit` and `offset` the way the oracle does: `offset` only counts
/// when there is a `limit`.
pub fn page<T>(rows: Vec<T>, limit: Option<usize>, offset: Option<usize>) -> Vec<T> {
    match limit {
        None => rows,
        Some(limit) => rows
            .into_iter()
            .skip(offset.unwrap_or(0))
            .take(limit)
            .collect(),
    }
}

/// A convenience for a backend whose rows are all at hand: filter, sort and
/// page `items` by `q`. Rows equal on every sort key (or all of them, with no
/// sort) keep the order they were given in.
pub fn run_query<'a>(
    items: impl IntoIterator<Item = Row<'a>>,
    q: &ItemQuery,
    graph: &dyn Graph,
) -> Vec<&'a Item> {
    let mut hits: Vec<&Item> = items
        .into_iter()
        .filter(|row| matches_query(*row, q, graph))
        .map(|row| row.item)
        .collect();
    sort_rows(&mut hits, &q.sort, |item| item);
    page(hits, q.limit, q.offset)
}

/// How many rows satisfy the query's schema and predicates (sort and paging
/// ignored, as `count` is in the oracle).
pub fn count_matching<'a>(
    items: impl IntoIterator<Item = Row<'a>>,
    q: &ItemQuery,
    graph: &dyn Graph,
) -> usize {
    items
        .into_iter()
        .filter(|row| matches_query(*row, q, graph))
        .count()
}

/// The references of `item` that follow `edge_types` (empty means all), as
/// `neighbors` walks them.
pub fn follows<'a>(
    item: &'a Item,
    edge_types: &'a [EdgeType],
) -> impl Iterator<Item = &'a TypedReference> {
    item.references
        .iter()
        .filter(move |r| edge_types.is_empty() || edge_types.contains(&r.edge_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use impress_item::{FlagState, Priority, SchemaRef, Visibility};

    fn item(title: &str) -> Item {
        let mut payload = BTreeMap::new();
        payload.insert("title".into(), Value::String(title.into()));
        Item {
            id: uuid::Uuid::new_v4(),
            schema: SchemaRef::from_stored("test".into()),
            payload,
            created: Utc::now(),
            modified: Utc::now(),
            author: "a".into(),
            author_kind: ActorKind::Human,
            logical_clock: 1,
            origin: None,
            canonical_id: None,
            tags: vec![],
            flag: None,
            is_read: false,
            is_starred: false,
            priority: Priority::Normal,
            visibility: Visibility::Private,
            message_type: None,
            produced_by: None,
            version: None,
            batch_id: None,
            references: vec![],
            parent: None,
            extra: Default::default(),
        }
    }

    fn yes(item: &Item, p: &Predicate) -> bool {
        matches(Row::of(item), p, &NoGraph)
    }

    #[test]
    fn missing_fields_are_null_not_false() {
        let it = item("t");
        let eq = Predicate::Eq("payload.nope".into(), Value::Int(1));
        let neq = Predicate::Neq("payload.nope".into(), Value::Int(1));
        assert_eq!(eval(Row::of(&it), &eq, &NoGraph), None);
        assert_eq!(eval(Row::of(&it), &neq, &NoGraph), None);
        // NOT NULL is NULL: the item is excluded both ways.
        assert!(!yes(&it, &Predicate::Not(Box::new(eq))));
        assert!(!yes(
            &it,
            &Predicate::Not(Box::new(Predicate::HasFlag(Some("red".into()))))
        ));
        // ...but a false branch decides an OR/AND around the NULL.
        assert!(yes(
            &it,
            &Predicate::Or(vec![neq.clone(), Predicate::IsRead(false)])
        ));
        assert!(!yes(
            &it,
            &Predicate::And(vec![neq, Predicate::IsRead(false)])
        ));
    }

    #[test]
    fn empty_in_is_false_and_its_negation_true() {
        let it = item("t");
        let empty = Predicate::In("id".into(), vec![]);
        assert!(!yes(&it, &empty));
        assert!(yes(&it, &Predicate::Not(Box::new(empty))));
        assert!(yes(&it, &Predicate::And(vec![])));
        assert!(!yes(&it, &Predicate::Or(vec![])));
    }

    #[test]
    fn in_with_a_null_member_is_unknown_unless_something_matches() {
        let mut it = item("t");
        it.payload.insert("n".into(), Value::Int(2));
        let none = Predicate::In("n".into(), vec![Value::Int(1), Value::Null]);
        assert_eq!(eval(Row::of(&it), &none, &NoGraph), None);
        let some = Predicate::In("n".into(), vec![Value::Int(2), Value::Null]);
        assert!(yes(&it, &some));
    }

    #[test]
    fn numbers_compare_across_int_and_real_and_sort_below_text() {
        let mut it = item("t");
        it.payload.insert("n".into(), Value::Int(2));
        it.payload.insert("f".into(), Value::Float(2.5));
        it.payload.insert("flag".into(), Value::Bool(true));
        assert!(yes(&it, &Predicate::Lt("n".into(), Value::Float(2.5))));
        assert!(yes(&it, &Predicate::Gt("f".into(), Value::Int(2))));
        assert!(yes(&it, &Predicate::Eq("flag".into(), Value::Bool(true))));
        assert!(yes(&it, &Predicate::Eq("flag".into(), Value::Int(1))));
        // A number is less than any text, whatever the text says.
        assert!(yes(
            &it,
            &Predicate::Lt("n".into(), Value::String("0".into()))
        ));
    }

    #[test]
    fn payload_paths_walk_objects_and_arrays() {
        let mut it = item("t");
        let mut inner = BTreeMap::new();
        inner.insert("k".into(), Value::Array(vec![Value::Int(7), Value::Int(8)]));
        it.payload.insert("o".into(), Value::Object(inner));
        assert!(yes(
            &it,
            &Predicate::Eq("payload.o.k[1]".into(), Value::Int(8))
        ));
        assert!(yes(&it, &Predicate::Eq("o.k[0]".into(), Value::Int(7))));
        assert_eq!(field_value(Row::of(&it), "payload.o.k[9]"), Scalar::Null);
        // An unsafe path is NULL, not an injection.
        assert_eq!(
            field_value(Row::of(&it), "payload.o'); drop table items; --"),
            Scalar::Null
        );
    }

    #[test]
    fn contains_is_full_text_on_five_fields_and_like_elsewhere() {
        let mut it = item("Dark Matter in Galaxy Clusters");
        it.payload
            .insert("doi".into(), Value::String("10.1234/Test_X".into()));
        let c = |it: &Item, f: &str, t: &str| yes(it, &Predicate::Contains(f.into(), t.into()));
        assert!(c(&it, "title", "Dark Matter"));
        assert!(c(&it, "title", "dark mat"));
        assert!(c(&it, "payload.title", "galaxy clust"));
        assert!(
            !c(&it, "title", "atter"),
            "full-text matches word prefixes only"
        );
        assert!(!c(&it, "title", "matter dark"), "the phrase is in order");
        // LIKE: substring, case-insensitive, `_` and `%` in the needle are literal.
        assert!(c(&it, "doi", "1234/test"));
        assert!(c(&it, "doi", "t_x"));
        assert!(!c(&it, "doi", "t.x"));
        assert!(!c(&it, "doi", "t%x"));
        assert!(!c(&it, "nope", "x"), "NULL operand");
        // Not column-restricted: the oracle's MATCH searches all five.
        it.payload
            .insert("abstract_text".into(), Value::String("neutrino".into()));
        assert!(c(&it, "title", "neutrino"));
    }

    #[test]
    fn has_tag_matches_the_tag_and_what_is_below_it() {
        let mut it = item("t");
        it.tags = vec!["methods/sims".into(), "topics".into()];
        assert!(yes(&it, &Predicate::HasTag("methods".into())));
        assert!(yes(&it, &Predicate::HasTag("methods/sims".into())));
        assert!(yes(&it, &Predicate::HasTag("topics".into())));
        assert!(!yes(&it, &Predicate::HasTag("method".into())));
        assert!(!yes(&it, &Predicate::HasTag("sims".into())));
    }

    #[test]
    fn envelope_columns_are_addressable() {
        let mut it = item("t");
        it.flag = Some(FlagState {
            color: "red".into(),
            style: None,
            length: None,
        });
        assert!(yes(
            &it,
            &Predicate::Eq("flag_color".into(), Value::String("red".into()))
        ));
        assert!(yes(
            &it,
            &Predicate::Eq("author_kind".into(), Value::String("human".into()))
        ));
        assert!(yes(
            &it,
            &Predicate::Eq("priority".into(), Value::String("normal".into()))
        ));
        assert!(yes(
            &it,
            &Predicate::Eq("schema".into(), Value::String("test".into()))
        ));
        assert!(yes(
            &it,
            &Predicate::Eq("is_read".into(), Value::Bool(false))
        ));
    }

    #[test]
    fn sorting_puts_null_first_ascending_and_last_descending() {
        let mut a = item("a");
        a.payload.insert("n".into(), Value::Int(1));
        let mut b = item("b");
        b.payload.insert("n".into(), Value::Int(2));
        let c = item("c");
        let asc = [SortDescriptor {
            field: "payload.n".into(),
            ascending: true,
        }];
        let desc = [SortDescriptor {
            field: "n".into(),
            ascending: false,
        }];
        let mut rows = vec![&b, &c, &a];
        sort_rows(&mut rows, &asc, |i| i);
        assert_eq!(
            rows.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![c.id, a.id, b.id]
        );
        sort_rows(&mut rows, &desc, |i| i);
        assert_eq!(
            rows.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![b.id, a.id, c.id]
        );
    }

    #[test]
    fn priority_is_not_a_sort_column() {
        let mut a = item("a");
        a.priority = Priority::Urgent;
        let b = item("b");
        let by_priority = [SortDescriptor {
            field: "priority".into(),
            ascending: true,
        }];
        // Both are NULL under the oracle's rule, so the order is the input's.
        assert_eq!(compare_items(&a, &b, &by_priority), Ordering::Equal);
    }

    #[test]
    fn offset_needs_a_limit() {
        assert_eq!(page(vec![1, 2, 3, 4], Some(2), Some(1)), vec![2, 3]);
        assert_eq!(page(vec![1, 2, 3, 4], None, Some(1)), vec![1, 2, 3, 4]);
        assert_eq!(page(vec![1, 2, 3, 4], Some(0), None), Vec::<i32>::new());
    }
}
