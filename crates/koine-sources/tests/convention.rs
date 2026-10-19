//! The pins of docs/plan-source-convention.md S-D1, S-D2, S-D4, S-D7.

use impress_item::kind;
use koine_sources::kind::{schema, Work, BIBLIOGRAPHIC_WORK};
use koine_sources::query::{INDEXES, RELATIONS};
use koine_sources::{ExplainError, ExplainRecord, Query, Status};
use serde_json::json;

/// The explain record of plan S-D1, verbatim.
const CROSSREF: &str = r#"{
  "source": "crossref", "name": "Crossref", "version": "rest-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match"],
  "indexes": {
    "any":      {"relations": ["any"]},
    "title":    {"relations": ["any", "adj"]},
    "author":   {"relations": ["any"]},
    "doi":      {"relations": ["="], "exact": true},
    "year":     {"relations": ["=", "<", ">", "within"]}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["doi"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "cited_by", "license"],
  "page": {"model": "cursor", "max": 1000},
  "etiquette": {"contact": "mailto", "limit": "headers", "key": "optional"},
  "secrets": ["crossref-plus-token"]
}"#;

fn crossref() -> ExplainRecord {
    ExplainRecord::from_json(CROSSREF).expect("S-D1 example parses and validates")
}

fn sample_work() -> serde_json::Value {
    json!({
        "type": "article-journal",
        "title": "Cosmic reionization on a grid",
        "author": [{"family": "Abel", "given": "Tom", "orcid": "0000-0002-1234-5678"},
                   {"family": "Smith"}],
        "issued": {"date-parts": [[2020, 5]]},
        "container-title": "ApJ",
        "volume": "900", "page": "1-12",
        "DOI": "10.1234/abc",
        "ids": {"doi": "10.1234/abc", "arxiv": "2001.00001"},
        "locations": [
            {"source": "arxiv", "source_id": "2001.00001", "version": "submitted", "primary": false,
             "pdf": "https://arxiv.org/pdf/2001.00001"},
            {"source": "crossref", "source_id": "10.1234/abc", "version": "published",
             "url": "https://doi.org/10.1234/abc", "primary": true}],
        "provenance": {"fetched_at": "2026-10-19T10:00:00Z", "source": "crossref",
                       "source_id": "10.1234/abc", "query": "reionization"},
        "metrics": {"cited_by": {"value": 42, "source": "crossref"}}
    })
}

#[test]
fn the_s_d1_example_parses_and_validates() {
    let r = crossref();
    assert_eq!(r.indexes["doi"].exact, Some(true));
    assert_eq!(r.secrets, ["crossref-plus-token"]);
    // It round-trips as JSON.
    let again: ExplainRecord = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
    assert_eq!(again, r);
}

#[test]
fn an_explain_record_outside_the_vocabulary_is_refused() {
    let mut r = crossref();
    r.indexes.insert(
        "fulltext".into(),
        serde_json::from_value(json!({"relations": ["any"]})).unwrap(),
    );
    assert_eq!(
        r.validate(),
        Err(ExplainError::UnknownIndex("fulltext".into()))
    );
    let mut r = crossref();
    r.indexes
        .get_mut("year")
        .unwrap()
        .relations
        .push("near".into());
    assert!(matches!(
        r.validate(),
        Err(ExplainError::UnknownRelation { .. })
    ));
}

#[test]
fn a_first_author_query_against_a_record_lacking_it_is_one_unsupported_diagnostic() {
    let q: Query = serde_json::from_value(json!({"and": [
        {"index": "first_author", "relation": "any", "term": "Abel"},
        {"index": "year", "relation": "within", "term": "2020 2024"},
        {"or": [{"index": "title", "relation": "any", "term": "reionization"},
                {"index": "any", "relation": "any", "term": "21cm"}]}
    ]}))
    .unwrap();
    q.validate().unwrap();
    let d = q.covered_by(&crossref());
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].status, Status::Unsupported);
    assert_eq!(d[0].source, "crossref");
    assert_eq!(d[0].detail, "bib.first_author");
    assert_eq!(
        serde_json::to_value(&d[0]).unwrap()["status"],
        "unsupported"
    );
}

#[test]
fn an_unsupported_relation_and_an_undeclared_native_are_named_too() {
    let q = Query::And {
        and: vec![
            Query::clause("author", "adj", "Abel"),
            Query::Not {
                not: Box::new(Query::Native {
                    native: "author:\"Abel, T\"".into(),
                }),
            },
        ],
    };
    let d = q.covered_by(&crossref());
    let details: Vec<_> = d.iter().map(|d| d.detail.as_str()).collect();
    assert_eq!(details, ["bib.author adj", "native"]);
    let mut ads = crossref();
    ads.native = Some("ads-lucene".into());
    assert_eq!(q.covered_by(&ads).len(), 1);
}

#[test]
fn a_covered_query_has_no_diagnostics() {
    let q = Query::clause("doi", "=", "10.1234/abc");
    assert!(q.covered_by(&crossref()).is_empty());
}

#[test]
fn queries_are_the_documented_json() {
    let text = r#"{"and":[{"index":"author","relation":"any","term":"Abel"},{"native":"x"}]}"#;
    let q: Query = serde_json::from_str(text).unwrap();
    assert_eq!(serde_json::to_string(&q).unwrap(), text);
    let bad = Query::clause("fulltext", "any", "x");
    assert!(bad.validate().is_err());
    assert!(Query::clause("author", "near", "x").validate().is_err());
}

#[test]
fn the_context_set_is_the_plans() {
    assert_eq!(INDEXES.len(), 19);
    assert_eq!(RELATIONS.len(), 8);
}

#[test]
fn the_kind_schema_validates_a_sample_and_registers() {
    kind::register(schema()).unwrap();
    let work: Work = serde_json::from_value(sample_work()).unwrap();
    work.validate().unwrap();
    let payload = work.to_payload();
    assert_eq!(kind::validate(&schema(), &payload), Vec::<String>::new());
    assert!(!kind::validate(&schema(), &Default::default()).is_empty()); // `type` is required
    koine_sources::ensure_registered();
    koine_sources::ensure_registered(); // idempotent
    assert_eq!(
        kind::version_of(BIBLIOGRAPHIC_WORK).as_deref(),
        Some("1.0.0")
    );
}

#[test]
fn a_bad_location_version_is_refused() {
    let mut v = sample_work();
    v["locations"][0]["version"] = json!("preprint");
    assert!(serde_json::from_value::<Work>(v).is_err());
    let mut v = sample_work();
    v["ids"]["scopus"] = json!("1");
    let work: Work = serde_json::from_value(v).unwrap();
    assert!(work.validate().unwrap_err().contains("scopus"));
}

#[test]
fn the_work_round_trips() {
    let work: Work = serde_json::from_value(sample_work()).unwrap();
    assert_eq!(serde_json::to_value(&work).unwrap(), sample_work());
}

#[test]
fn to_csl_json_validates_against_csl_data() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/csl-data.json")).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let work: Work = serde_json::from_value(sample_work()).unwrap();
    let csl = work.to_csl_json();
    let errors: Vec<_> = validator
        .iter_errors(&json!([csl.clone()]))
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}\n{csl}");
    assert_eq!(csl["id"], "10.1234/abc");
    assert_eq!(csl["custom"]["ids"]["arxiv"], "2001.00001");
    assert_eq!(csl["custom"]["author_orcid"][0], "0000-0002-1234-5678");
    assert!(csl["author"][0].get("orcid").is_none());
    // A bare minimum is still strict CSL.
    let bare = Work {
        ty: "book".into(),
        ..Default::default()
    };
    assert!(validator.is_valid(&json!([bare.to_csl_json()])));
}

#[test]
fn the_schemas_are_emitted() {
    for s in [
        schemars::schema_for!(ExplainRecord),
        schemars::schema_for!(Query),
        schemars::schema_for!(Work),
    ] {
        assert!(serde_json::to_string(&s).unwrap().len() > 100);
    }
}
