//! Cases for `query` and `count`: the predicates, sorting and paging.
//!
//! They share one small corpus, so a predicate's expected answer can be read
//! off the table below.
//!
//! | title                             | n    | score | doi            | tags                      | flag | read | star | parent | created | modified |
//! |-----------------------------------|------|-------|----------------|---------------------------|------|------|------|--------|---------|----------|
//! | A "Dark Matter in Galaxy Clusters"| 1    | 1.5   | 10.1234/Alpha  | methods/sims              | red  | yes  | no   |        | day 3   | day 9    |
//! | B "Stellar Populations"           | 2    | 2.5   | 10.1234/Beta   | methods/obs topics/stars  | blue | no   | yes  | F      | day 1   | day 8    |
//! | C "Galaxy Formation"              | 3    |       |                | topics/cosmo              |      | yes  | yes  | F      | day 4   | day 7    |
//! | D "Cosmic Voids"                  |      |       |                |                           |      | no   | no   |        | day 2   | day 6    |
//!
//! A cites B; C cites B and mentions A. A, B, C, D are schema `q/doc`; F is a
//! `q/folder`; E (`q/other`, "Dark Matter Elsewhere") proves schema scoping.
//! B is inserted before A, A before C, C before D, so the clock order is
//! B, A, C, D.

use std::collections::BTreeSet;

use impress_item::{
    AttributedStore, EdgeType, FlagState, ItemId, ItemQuery, Predicate, SortDescriptor, Value,
};

use crate::fixture::*;

const DARK: &str = "Dark Matter in Galaxy Clusters";
const STELLAR: &str = "Stellar Populations";
const GALAXY: &str = "Galaxy Formation";
const COSMIC: &str = "Cosmic Voids";

struct Corpus {
    a: ItemId,
    b: ItemId,
    c: ItemId,
    d: ItemId,
    f: ItemId,
}

fn flag(color: &str) -> Option<FlagState> {
    Some(FlagState {
        color: color.into(),
        style: None,
        length: None,
    })
}

fn corpus(store: &dyn AttributedStore) -> Corpus {
    let mut folder = item("q/folder", "Folder");
    folder.created = day(1);
    let f = store.insert(folder).unwrap();

    let mut b = item("q/doc", STELLAR);
    b.payload.insert("n".into(), Value::Int(2));
    b.payload.insert("score".into(), Value::Float(2.5));
    b.payload.insert("doi".into(), text("10.1234/Beta"));
    b.tags = vec!["methods/obs".into(), "topics/stars".into()];
    b.flag = flag("blue");
    b.is_starred = true;
    b.parent = Some(f);
    b.created = day(1);
    b.modified = day(8);
    let b = store.insert(b).unwrap();

    let mut a = item("q/doc", DARK);
    a.payload.insert("n".into(), Value::Int(1));
    a.payload.insert("score".into(), Value::Float(1.5));
    a.payload.insert("doi".into(), text("10.1234/Alpha"));
    a.payload
        .insert("abstract_text".into(), text("We study halos"));
    a.tags = vec!["methods/sims".into()];
    a.flag = flag("red");
    a.is_read = true;
    a.created = day(3);
    a.modified = day(9);
    a.references = vec![reference(b, EdgeType::Cites)];
    let a = store.insert(a).unwrap();

    let mut c = item("q/doc", GALAXY);
    c.payload.insert("n".into(), Value::Int(3));
    c.tags = vec!["topics/cosmo".into()];
    c.is_read = true;
    c.is_starred = true;
    c.parent = Some(f);
    c.created = day(4);
    c.modified = day(7);
    c.references = vec![
        reference(b, EdgeType::Cites),
        reference(a, EdgeType::Mentions),
    ];
    let c = store.insert(c).unwrap();

    let mut d = item("q/doc", COSMIC);
    d.created = day(2);
    d.modified = day(6);
    let d = store.insert(d).unwrap();

    let mut e = item("q/other", "Dark Matter Elsewhere");
    e.payload.insert("n".into(), Value::Int(1));
    store.insert(e).unwrap();

    Corpus { a, b, c, d, f }
}

/// The titles `q/doc` rows matching `predicates` have, as a set.
fn hits(store: &dyn AttributedStore, predicates: Vec<Predicate>) -> BTreeSet<String> {
    title_set(&store.query(&where_("q/doc", predicates)).unwrap())
}

fn ordered(store: &dyn AttributedStore, q: ItemQuery) -> Vec<String> {
    titles(&store.query(&q).unwrap())
}

fn eq(field: &str, v: Value) -> Predicate {
    Predicate::Eq(field.into(), v)
}

/// The schema constraint is exact equality: no prefix, no version tolerance,
/// no case folding, no namespace fallback. A reader that spells a ref
/// differently from its writer gets zero rows, silently. This case pins that
/// down explicitly, because it is the one a backend can break by being
/// helpful.
pub fn query_schema_is_exact_equality(store: &dyn AttributedStore) {
    for (schema_name, title) in [
        ("imbib/library", "namespaced"),
        ("imbib/library@1.0.0", "versioned"),
        ("library", "bare"),
        ("Imbib/Library", "capitalised"),
        ("task@1.0.0", "task"),
    ] {
        insert(store, schema_name, title);
    }
    let only = |schema_name: &str| titles(&store.query(&all_of(schema_name)).unwrap());
    assert_eq!(only("imbib/library"), vec!["namespaced"]);
    assert_eq!(only("imbib/library@1.0.0"), vec!["versioned"]);
    assert_eq!(only("library"), vec!["bare"]);
    assert_eq!(only("Imbib/Library"), vec!["capitalised"]);
    assert_eq!(only("task@1.0.0"), vec!["task"]);

    // Spelled differently from every writer: zero rows, and no error.
    for misspelt in [
        "imbib/librar",
        "imbib",
        "imbib/",
        "task",
        "task@1.0",
        "task@1.0.0 ",
        "imbib/library@",
        "imbib/library@2.0.0",
        "IMBIB/LIBRARY",
    ] {
        assert!(only(misspelt).is_empty(), "{misspelt:?} must match nothing");
        assert_eq!(store.count(&all_of(misspelt)).unwrap(), 0);
    }
    // `None` is every kind.
    assert_eq!(store.query(&ItemQuery::default()).unwrap().len(), 5);
}

/// `Eq` and `Neq` on payload paths, on the item's own columns, across ints,
/// reals, strings and booleans.
pub fn query_eq_and_neq(store: &dyn AttributedStore) {
    corpus(store);
    assert_eq!(
        hits(store, vec![eq("payload.n", Value::Int(2))]),
        set(&[STELLAR])
    );
    assert_eq!(
        hits(store, vec![eq("n", Value::Int(2))]),
        set(&[STELLAR]),
        "a bare name is a payload path"
    );
    assert_eq!(hits(store, vec![eq("title", text(COSMIC))]), set(&[COSMIC]));
    assert_eq!(
        hits(store, vec![eq("score", Value::Float(1.5))]),
        set(&[DARK])
    );
    assert_eq!(
        hits(store, vec![eq("score", Value::Int(2))]),
        set(&[]),
        "2.5 is not 2"
    );
    assert_eq!(
        hits(store, vec![eq("is_read", Value::Bool(true))]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(
        hits(store, vec![eq("is_starred", Value::Int(1))]),
        set(&[STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![eq("flag_color", text("red"))]),
        set(&[DARK])
    );
    assert_eq!(hits(store, vec![eq("author_kind", text("human"))]).len(), 4);
    assert_eq!(hits(store, vec![eq("priority", text("normal"))]).len(), 4);
    assert_eq!(hits(store, vec![eq("schema", text("q/doc"))]).len(), 4);
    assert_eq!(
        hits(store, vec![eq("author", text("nobody@example.com"))]),
        set(&[])
    );

    // Neq is "has the field, and it differs".
    assert_eq!(
        hits(store, vec![Predicate::Neq("n".into(), Value::Int(2))]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(
        hits(
            store,
            vec![Predicate::Neq("is_read".into(), Value::Bool(true))]
        ),
        set(&[STELLAR, COSMIC])
    );
}

/// `Gt`, `Lt`, `Gte`, `Lte`: integers and reals compare as numbers with each
/// other, text compares bytewise, `created` and `modified` are epoch
/// milliseconds.
pub fn query_ordering_comparisons(store: &dyn AttributedStore) {
    corpus(store);
    let int = Value::Int;
    assert_eq!(
        hits(store, vec![Predicate::Gt("n".into(), int(1))]),
        set(&[STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::Lt("n".into(), int(3))]),
        set(&[DARK, STELLAR])
    );
    assert_eq!(
        hits(store, vec![Predicate::Gte("n".into(), int(2))]),
        set(&[STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::Lte("n".into(), int(2))]),
        set(&[DARK, STELLAR])
    );
    // An integer field against a real bound and the other way round.
    assert_eq!(
        hits(store, vec![Predicate::Gt("n".into(), Value::Float(1.5))]),
        set(&[STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::Gt("score".into(), int(2))]),
        set(&[STELLAR])
    );
    assert_eq!(
        hits(
            store,
            vec![Predicate::Lte("score".into(), Value::Float(1.5))]
        ),
        set(&[DARK])
    );
    // Text.
    assert_eq!(
        hits(
            store,
            vec![Predicate::Gt("doi".into(), text("10.1234/Alpha"))]
        ),
        set(&[STELLAR])
    );
    assert_eq!(
        hits(
            store,
            vec![Predicate::Lt("doi".into(), text("10.1234/Beta"))]
        ),
        set(&[DARK])
    );
    // Timestamps are milliseconds.
    let ms = |d| int(day(d).timestamp_millis());
    assert_eq!(
        hits(store, vec![Predicate::Gt("created".into(), ms(2))]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::Gte("created".into(), ms(2))]),
        set(&[DARK, GALAXY, COSMIC])
    );
    assert_eq!(
        hits(store, vec![Predicate::Lt("modified".into(), ms(8))]),
        set(&[GALAXY, COSMIC])
    );
    assert_eq!(
        hits(store, vec![Predicate::Lte("modified".into(), ms(8))]),
        set(&[GALAXY, COSMIC, STELLAR])
    );
}

/// `Contains`: on the five prose fields (`title`, `author_text`,
/// `abstract_text`, `note`, `body_content`) it is word search, case-blind,
/// phrase-ordered, the last word a prefix. On every other field it is a
/// case-blind substring.
pub fn query_contains(store: &dyn AttributedStore) {
    corpus(store);
    let contains = |f: &str, t: &str| hits(store, vec![Predicate::Contains(f.into(), t.into())]);
    assert_eq!(contains("title", "galaxy"), set(&[DARK, GALAXY]));
    assert_eq!(contains("title", "DARK MATTER"), set(&[DARK]));
    assert_eq!(
        contains("payload.title", "dark mat"),
        set(&[DARK]),
        "the last word is a prefix"
    );
    assert_eq!(contains("title", "galaxy form"), set(&[GALAXY]));
    assert_eq!(
        contains("title", "matter dark"),
        set(&[]),
        "the phrase is in order"
    );
    assert_eq!(
        contains("title", "atter"),
        set(&[]),
        "words match by prefix, not by substring"
    );
    assert_eq!(contains("title", "nonexistent"), set(&[]));
    assert_eq!(contains("abstract_text", "halos"), set(&[DARK]));
    // Other fields: substring, ignoring case.
    assert_eq!(contains("doi", "alpha"), set(&[DARK]));
    assert_eq!(contains("doi", "1234"), set(&[DARK, STELLAR]));
    assert_eq!(contains("doi", "10.1234/"), set(&[DARK, STELLAR]));
    assert_eq!(contains("doi", "zzz"), set(&[]));
    assert_eq!(contains("doi", "a_pha"), set(&[]), "`_` is not a wildcard");
    assert_eq!(contains("doi", "10%"), set(&[]), "`%` is not a wildcard");
}

/// `In`: membership, `NULL` never a member, an empty set matches nothing.
pub fn query_in(store: &dyn AttributedStore) {
    corpus(store);
    let int = Value::Int;
    assert_eq!(
        hits(store, vec![Predicate::In("n".into(), vec![int(1), int(3)])]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(
        hits(
            store,
            vec![Predicate::In(
                "title".into(),
                vec![text(COSMIC), text("Nope")]
            )]
        ),
        set(&[COSMIC])
    );
    assert_eq!(
        hits(
            store,
            vec![Predicate::In("n".into(), vec![Value::Float(2.0), int(9)])]
        ),
        set(&[STELLAR]),
        "2.0 is 2"
    );
    assert_eq!(
        hits(store, vec![Predicate::In("n".into(), vec![])]),
        set(&[])
    );
    assert_eq!(
        hits(store, vec![Predicate::In("id".into(), vec![])]),
        set(&[]),
        "an empty set is FALSE, never every row"
    );
    // The negation of FALSE is every row.
    assert_eq!(
        hits(
            store,
            vec![Predicate::Not(Box::new(Predicate::In("n".into(), vec![])))]
        )
        .len(),
        4
    );
}

/// `HasTag` matches the tag and everything below it, on path boundaries.
pub fn query_has_tag(store: &dyn AttributedStore) {
    corpus(store);
    let tagged = |t: &str| hits(store, vec![Predicate::HasTag(t.into())]);
    assert_eq!(tagged("methods"), set(&[DARK, STELLAR]));
    assert_eq!(tagged("methods/sims"), set(&[DARK]));
    assert_eq!(tagged("topics"), set(&[STELLAR, GALAXY]));
    assert_eq!(tagged("topics/stars"), set(&[STELLAR]));
    assert_eq!(
        tagged("method"),
        set(&[]),
        "a prefix of a word is not an ancestor"
    );
    assert_eq!(tagged("sims"), set(&[]), "a descendant is not an ancestor");
    assert_eq!(tagged("nope"), set(&[]));
}

/// `HasFlag`, `IsRead`, `IsStarred`.
pub fn query_flags_read_and_starred(store: &dyn AttributedStore) {
    corpus(store);
    assert_eq!(
        hits(store, vec![Predicate::HasFlag(None)]),
        set(&[DARK, STELLAR])
    );
    assert_eq!(
        hits(store, vec![Predicate::HasFlag(Some("red".into()))]),
        set(&[DARK])
    );
    assert_eq!(
        hits(store, vec![Predicate::HasFlag(Some("blue".into()))]),
        set(&[STELLAR])
    );
    assert_eq!(
        hits(store, vec![Predicate::HasFlag(Some("green".into()))]),
        set(&[])
    );
    assert_eq!(
        hits(store, vec![Predicate::IsRead(true)]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::IsRead(false)]),
        set(&[STELLAR, COSMIC])
    );
    assert_eq!(
        hits(store, vec![Predicate::IsStarred(true)]),
        set(&[STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::IsStarred(false)]),
        set(&[DARK, COSMIC])
    );
}

/// `HasParent`.
pub fn query_has_parent(store: &dyn AttributedStore) {
    let k = corpus(store);
    assert_eq!(
        hits(store, vec![Predicate::HasParent(k.f)]),
        set(&[STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::HasParent(uuid::Uuid::new_v4())]),
        set(&[])
    );
    // `where_` scopes to q/doc; the folder is its own kind.
    assert!(store
        .query(&where_("q/folder", vec![Predicate::HasParent(k.f)]))
        .unwrap()
        .is_empty());
}

/// `HasReference` (the row points at a target) and `ReferencedBy` (the row is
/// pointed at by a source), per edge type.
pub fn query_references_in_both_directions(store: &dyn AttributedStore) {
    let k = corpus(store);
    let has =
        |edge: EdgeType, target: ItemId| hits(store, vec![Predicate::HasReference(edge, target)]);
    let by =
        |edge: EdgeType, source: ItemId| hits(store, vec![Predicate::ReferencedBy(edge, source)]);

    assert_eq!(has(EdgeType::Cites, k.b), set(&[DARK, GALAXY]));
    assert_eq!(has(EdgeType::Mentions, k.a), set(&[GALAXY]));
    assert_eq!(has(EdgeType::Cites, k.a), set(&[]), "wrong edge type");
    assert_eq!(has(EdgeType::Mentions, k.b), set(&[]), "wrong edge type");
    assert_eq!(has(EdgeType::Cites, k.d), set(&[]));

    assert_eq!(by(EdgeType::Cites, k.a), set(&[STELLAR]));
    assert_eq!(by(EdgeType::Cites, k.c), set(&[STELLAR]));
    assert_eq!(by(EdgeType::Mentions, k.c), set(&[DARK]));
    assert_eq!(by(EdgeType::Cites, k.b), set(&[]), "B points at nothing");
    assert_eq!(by(EdgeType::Cites, k.d), set(&[]));
}

/// `And`, `Or`, `Not`, nested; the empty conjunction is true, the empty
/// disjunction false; separate predicates of one query are a conjunction.
pub fn query_logical_combinators(store: &dyn AttributedStore) {
    corpus(store);
    let read = || Predicate::IsRead(true);
    let star = || Predicate::IsStarred(true);
    assert_eq!(
        hits(store, vec![Predicate::And(vec![read(), star()])]),
        set(&[GALAXY])
    );
    assert_eq!(
        hits(store, vec![read(), star()]),
        set(&[GALAXY]),
        "predicates are ANDed"
    );
    assert_eq!(
        hits(store, vec![Predicate::Or(vec![read(), star()])]),
        set(&[DARK, STELLAR, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::Not(Box::new(read()))]),
        set(&[STELLAR, COSMIC])
    );
    assert_eq!(
        hits(
            store,
            vec![Predicate::And(vec![
                Predicate::Not(Box::new(read())),
                Predicate::Or(vec![
                    Predicate::HasTag("topics".into()),
                    Predicate::HasFlag(Some("red".into())),
                ]),
            ])]
        ),
        set(&[STELLAR])
    );
    assert_eq!(
        hits(store, vec![Predicate::And(vec![])]).len(),
        4,
        "the empty AND is true"
    );
    assert!(
        hits(store, vec![Predicate::Or(vec![])]).is_empty(),
        "the empty OR is false"
    );
    assert!(hits(
        store,
        vec![Predicate::Not(Box::new(Predicate::And(vec![])))]
    )
    .is_empty());
}

/// A comparison against a field the row does not have is neither true nor
/// false but unknown, and unknown is never returned, not even under `Not` or
/// `Neq`. (This is SQL's `NULL`; it is what a stored comparison means.)
pub fn query_missing_fields_are_unknown_not_false(store: &dyn AttributedStore) {
    corpus(store);
    let n2 = || eq("n", Value::Int(2));
    assert_eq!(hits(store, vec![n2()]), set(&[STELLAR]));
    // D has no `n`: it is in neither the answer nor its complement.
    assert_eq!(
        hits(store, vec![Predicate::Not(Box::new(n2()))]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(
        hits(store, vec![Predicate::Neq("n".into(), Value::Int(2))]),
        set(&[DARK, GALAXY])
    );
    assert_eq!(hits(store, vec![eq("nope", Value::Int(1))]), set(&[]));
    assert_eq!(
        hits(store, vec![Predicate::Neq("nope".into(), Value::Int(1))]),
        set(&[])
    );
    // Likewise a missing flag and a missing parent.
    assert_eq!(
        hits(
            store,
            vec![Predicate::Not(Box::new(Predicate::HasFlag(Some(
                "red".into()
            ))))]
        ),
        set(&[STELLAR]),
        "unflagged rows are unknown, not 'not red'"
    );
    // A known-false branch decides an OR/AND around an unknown.
    assert_eq!(
        hits(
            store,
            vec![Predicate::Or(vec![
                Predicate::Neq("n".into(), Value::Int(2)),
                Predicate::IsRead(false),
            ])]
        )
        .len(),
        4
    );
    assert!(hits(
        store,
        vec![Predicate::And(vec![
            Predicate::Neq("n".into(), Value::Int(2)),
            Predicate::IsRead(false),
        ])]
    )
    .is_empty());
}

/// Sorting by every sortable field, ascending and descending, by several
/// keys, and the placement of rows that lack the field.
pub fn query_sort_orders(store: &dyn AttributedStore) {
    let k = corpus(store);
    let by = |sort: &[(&str, bool)]| ordered(store, sorted("q/doc", sort));

    assert_eq!(by(&[("created", true)]), [STELLAR, COSMIC, DARK, GALAXY]);
    assert_eq!(by(&[("created", false)]), [GALAXY, DARK, COSMIC, STELLAR]);
    assert_eq!(by(&[("modified", true)]), [COSMIC, GALAXY, STELLAR, DARK]);
    assert_eq!(by(&[("modified", false)]), [DARK, STELLAR, GALAXY, COSMIC]);
    // Insertion order is B, A, C, D, and the clock follows it.
    assert_eq!(
        by(&[("logical_clock", true)]),
        [STELLAR, DARK, GALAXY, COSMIC]
    );
    assert_eq!(
        by(&[("logical_clock", false)]),
        [COSMIC, GALAXY, DARK, STELLAR]
    );
    assert_eq!(
        by(&[("payload.title", true)]),
        [COSMIC, DARK, GALAXY, STELLAR]
    );
    assert_eq!(by(&[("title", false)]), [STELLAR, GALAXY, DARK, COSMIC]);
    assert_eq!(
        by(&[("is_read", true), ("payload.title", true)]),
        [COSMIC, STELLAR, DARK, GALAXY]
    );
    assert_eq!(
        by(&[("is_read", false), ("payload.title", true)]),
        [DARK, GALAXY, COSMIC, STELLAR]
    );

    // By id: the order of the ids as text.
    let mut by_id = [(k.a, DARK), (k.b, STELLAR), (k.c, GALAXY), (k.d, COSMIC)];
    by_id.sort_by_key(|(id, _)| id.to_string());
    let expected: Vec<&str> = by_id.iter().map(|(_, t)| *t).collect();
    assert_eq!(by(&[("id", true)]), expected);
    let mut reversed = expected.clone();
    reversed.reverse();
    assert_eq!(by(&[("id", false)]), reversed);

    // A row without the field sorts before every row that has it, ascending,
    // and after them, descending. D has no `n`.
    assert_eq!(by(&[("payload.n", true)]), [COSMIC, DARK, STELLAR, GALAXY]);
    assert_eq!(by(&[("n", false)]), [GALAXY, STELLAR, DARK, COSMIC]);
    // Reals sort among themselves; rows tied on the first key fall to the next.
    assert_eq!(
        by(&[("score", true), ("title", true)]),
        [COSMIC, GALAXY, DARK, STELLAR]
    );
    assert_eq!(
        by(&[("score", false), ("title", true)]),
        [STELLAR, DARK, COSMIC, GALAXY]
    );
}

/// `limit` and `offset` page through a sorted result.
pub fn query_limit_and_offset(store: &dyn AttributedStore) {
    corpus(store);
    let page = |limit: Option<usize>, offset: Option<usize>| {
        ordered(
            store,
            ItemQuery {
                sort: vec![SortDescriptor {
                    field: "payload.title".into(),
                    ascending: true,
                }],
                limit,
                offset,
                ..all_of("q/doc")
            },
        )
    };
    // Sorted by title: Cosmic, Dark, Galaxy, Stellar.
    assert_eq!(page(Some(2), None), [COSMIC, DARK]);
    assert_eq!(page(Some(2), Some(1)), [DARK, GALAXY]);
    assert_eq!(page(Some(2), Some(3)), [STELLAR]);
    assert_eq!(page(Some(2), Some(4)), Vec::<String>::new());
    assert_eq!(page(Some(2), Some(40)), Vec::<String>::new());
    assert_eq!(page(Some(0), None), Vec::<String>::new());
    assert_eq!(page(Some(0), Some(1)), Vec::<String>::new());
    assert_eq!(page(Some(100), None), [COSMIC, DARK, GALAXY, STELLAR]);
    assert_eq!(page(Some(100), Some(2)), [GALAXY, STELLAR]);
    assert_eq!(page(None, None), [COSMIC, DARK, GALAXY, STELLAR]);
}

/// An `offset` with no `limit` is ignored: the result starts at the first
/// row. This is what the oracle does (it emits `OFFSET` only after `LIMIT`),
/// pinned so no backend is more helpful than another. If you decide it is a
/// bug, fix it in the oracle first and change this case with it.
pub fn query_offset_without_limit_is_ignored(store: &dyn AttributedStore) {
    corpus(store);
    let q = ItemQuery {
        sort: vec![SortDescriptor {
            field: "payload.title".into(),
            ascending: true,
        }],
        offset: Some(2),
        ..all_of("q/doc")
    };
    assert_eq!(ordered(store, q), [COSMIC, DARK, GALAXY, STELLAR]);
}

/// `count` is the number of rows `query` would return without paging.
pub fn count_matches_query(store: &dyn AttributedStore) {
    corpus(store);
    let queries = vec![
        all_of("q/doc"),
        all_of("q/other"),
        all_of("q/nothing"),
        where_("q/doc", vec![Predicate::IsRead(true)]),
        where_("q/doc", vec![Predicate::HasTag("topics".into())]),
        where_(
            "q/doc",
            vec![Predicate::Contains("title".into(), "galaxy".into())],
        ),
        where_(
            "q/doc",
            vec![Predicate::Not(Box::new(eq("n", Value::Int(2))))],
        ),
        where_(
            "q/doc",
            vec![Predicate::Or(vec![
                Predicate::IsStarred(true),
                Predicate::HasFlag(None),
            ])],
        ),
    ];
    for q in queries {
        assert_eq!(
            store.count(&q).unwrap(),
            store.query(&q).unwrap().len(),
            "count and query disagree for {q:?}"
        );
    }
    assert_eq!(store.count(&all_of("q/doc")).unwrap(), 4);
    assert_eq!(store.count(&all_of("q/nothing")).unwrap(), 0);
}

/// `count` counts matches; it does not stop at `limit` or skip `offset`.
pub fn count_ignores_limit_and_offset(store: &dyn AttributedStore) {
    corpus(store);
    let paged = ItemQuery {
        limit: Some(1),
        offset: Some(1),
        ..all_of("q/doc")
    };
    assert_eq!(store.query(&paged).unwrap().len(), 1);
    assert_eq!(store.count(&paged).unwrap(), 4);
}

/// `include_tags: false` and `include_references: false` leave those fields
/// empty on query results (the caller said it does not need them); `get`
/// always has them.
pub fn query_include_flags_omit_tags_and_references(store: &dyn AttributedStore) {
    let k = corpus(store);
    let find = |q: ItemQuery| {
        store
            .query(&q)
            .unwrap()
            .into_iter()
            .find(|i| i.id == k.c)
            .expect("C is in the result")
    };
    let full = find(all_of("q/doc"));
    assert_eq!(full.tags, vec!["topics/cosmo".to_owned()]);
    assert_eq!(full.references.len(), 2);

    let lean = find(ItemQuery {
        include_tags: false,
        include_references: false,
        ..all_of("q/doc")
    });
    assert!(lean.tags.is_empty());
    assert!(lean.references.is_empty());
    assert_eq!(
        lean.payload, full.payload,
        "everything else is the same row"
    );

    let only_tags = find(ItemQuery {
        include_references: false,
        ..all_of("q/doc")
    });
    assert_eq!(only_tags.tags.len(), 1);
    assert!(only_tags.references.is_empty());

    let got = store.get(k.c).unwrap().unwrap();
    assert_eq!(got.tags.len(), 1);
    assert_eq!(got.references.len(), 2);
}

/// Every row `query` returns is the row `get` returns (up to the optional
/// tags and references the flags above control).
pub fn query_rows_are_the_rows_get_returns(store: &dyn AttributedStore) {
    corpus(store);
    for row in store.query(&all_of("q/doc")).unwrap() {
        let got = store.get(row.id).unwrap().unwrap();
        assert_eq!(row.payload, got.payload);
        assert_eq!(row.logical_clock, got.logical_clock);
        assert_eq!(row.parent, got.parent);
        assert_eq!(row.flag, got.flag);
        assert_eq!(row.is_read, got.is_read);
        assert_eq!(
            row.created.timestamp_millis(),
            got.created.timestamp_millis()
        );
    }
}
