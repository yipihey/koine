//! Tier A: every verb is found in the linked inventory and run through the
//! pipeline (docs/capability.md § 8); the examples are offline by
//! construction (an uncovered index, a non-identifier, an empty list). The
//! adapter-level behaviour is pinned with a stub source that never leaves
//! the process.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use impress_service_core::call;
use impress_service_core::descriptor_handle::VerbHandle;
use serde_json::{json, Value};

use crate::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use crate::dto::MatchInput;
use crate::registry::Registry;
use crate::service::{DefaultSourceService, SourceService};
use crate::types::{
    Diagnostic, DiagnosticStatus, EtiquetteInfo, ExplainRecord, IndexInfo, Location, PageInfo,
    Query, Work,
};

/// A source that answers from memory: `author` and `title` indexes, DOIs of
/// the form `10.5555/*`, one page of `n` rows then a cursor.
struct Stub {
    source: &'static str,
    calls: AtomicUsize,
}

impl Stub {
    fn new(source: &'static str) -> Arc<Self> {
        Arc::new(Stub {
            source,
            calls: AtomicUsize::new(0),
        })
    }

    fn work(&self, doi: &str, title: &str) -> Work {
        Work {
            id: format!("doi:{doi}"),
            source: self.source.into(),
            source_id: doi.into(),
            title: Some(title.into()),
            ids: BTreeMap::from([("doi".to_string(), doi.to_string())]),
            locations: vec![Location {
                source: self.source.into(),
                source_id: doi.into(),
                version: "published".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}

#[async_trait]
impl Adapter for Stub {
    fn explain(&self) -> ExplainRecord {
        ExplainRecord {
            source: self.source.into(),
            name: "Stub".into(),
            version: "0".into(),
            kind: "bibliographic-work@1".into(),
            operations: ["search", "fetch", "match", "related", "export"]
                .map(String::from)
                .to_vec(),
            indexes: BTreeMap::from([
                (
                    "author".into(),
                    IndexInfo {
                        relations: vec!["any".into()],
                        exact: false,
                    },
                ),
                (
                    "title".into(),
                    IndexInfo {
                        relations: vec!["any".into()],
                        exact: false,
                    },
                ),
                (
                    "doi".into(),
                    IndexInfo {
                        relations: vec!["=".into()],
                        exact: true,
                    },
                ),
            ]),
            sortable: vec!["relevance".into(), "year".into()],
            identifiers: vec!["doi".into()],
            fills: vec!["title".into(), "doi".into()],
            page: PageInfo {
                model: "cursor".into(),
                max: 50,
            },
            etiquette: EtiquetteInfo {
                contact: "none".into(),
                limit: "none".into(),
                key: "none".into(),
            },
            secrets: vec![],
            native: None,
        }
    }

    fn detect(&self, id: &str) -> bool {
        id.starts_with("doi:10.5555/")
    }

    async fn search(&self, query: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let record = self.explain();
        let mut out = Page::default();
        for clause in query.clauses() {
            if !record.covers(clause.index, Some(clause.relation)) {
                out.diagnostics
                    .push(Diagnostic::unsupported(self.source, clause.index));
            }
        }
        let start: usize = page.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        for i in start..(start + page.limit as usize).min(start + 3) {
            out.rows.push(self.work(&format!("10.5555/{i}"), &format!("Row {i}")));
        }
        out.next = (start + 3 < 6).then(|| (start + 3).to_string());
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let doi = id.trim_start_matches("doi:");
        Ok((doi == "10.5555/1").then(|| self.work(doi, &format!("One from {}", self.source))))
    }

    async fn related(&self, _id: &str, relation: Relation, _page: PageRequest) -> Result<Page, Diagnostic> {
        match relation {
            Relation::Cites => Ok(Page {
                rows: vec![self.work("10.5555/2", "Two")],
                ..Default::default()
            }),
            other => Err(Diagnostic::new(self.source, DiagnosticStatus::Unsupported)
                .with_message(format!("{} cannot walk {}", self.source, other.as_str()))),
        }
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        Ok(match format {
            ExportFormat::CslJson => serde_json::to_string(
                &ids.iter()
                    .map(|id| self.work(id.trim_start_matches("doi:"), "X").csl_json())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            _ => ids
                .iter()
                .map(|id| format!("@article{{{id}, title={{X}}}}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
        })
    }
}

fn service() -> (DefaultSourceService, Arc<Stub>) {
    let stub = Stub::new("stub");
    let registry = Registry::new();
    registry.register(stub.clone());
    (DefaultSourceService::with_registry(registry), stub)
}

fn contains(haystack: &Value, needle: &Value) -> bool {
    match (haystack, needle) {
        (Value::Object(h), Value::Object(n)) => n.iter().all(|(k, v)| h.get(k).is_some_and(|hv| contains(hv, v))),
        (Value::Array(h), Value::Array(n)) => h.len() == n.len() && h.iter().zip(n).all(|(a, b)| contains(a, b)),
        _ => haystack == needle,
    }
}

#[test]
fn every_verb_is_documented_has_an_example_and_the_example_answers_offline() {
    let verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == "source-service")
        .collect();
    let mut names: Vec<&str> = verbs.iter().map(|v| v.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "source-service_explain",
            "source-service_export",
            "source-service_fetch",
            "source-service_match-citation",
            "source-service_related",
            "source-service_search",
        ]
    );
    for verb in &verbs {
        assert!(!verb.description().is_empty(), "{}", verb.name());
        assert!(!verb.examples().is_empty(), "{} has no example", verb.name());
        assert!(
            verb.effects().reach.iter().any(|r| matches!(r, impress_service_core::descriptor::Reach::Network)),
            "{} must declare the network reach",
            verb.name()
        );
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value()).unwrap_or_else(|e| panic!("{label}: {e}"));
            let expect: Value = serde_json::from_str(example.expect.expect("every example states its expect")).unwrap();
            assert!(contains(&result, &expect), "{label}: {result} vs {expect}");
        }
    }
}

#[test]
fn the_plan_spelling_of_match_resolves_through_the_alias() {
    let result = call::call("source-service_match", json!({"citation": {"text": ""}})).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"));
}

#[test]
fn a_misspelt_argument_is_refused_before_the_handler() {
    let result = call::call("source-service_search", json!({"querry": {}})).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
}

#[test]
fn a_refused_search_carries_only_the_outcome() {
    let result = call::call("source-service_search", json!({"query": {}})).unwrap();
    assert_eq!(result["ok"], json!(false));
    assert!(result.get("rows").is_none(), "{result}");
    let result = call::call("source-service_search", json!({"query": {"index": "title", "relation": "any", "term": "x"}, "limit": 101})).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"));
}

#[tokio::test]
async fn explain_is_the_inventory() {
    let (service, _) = service();
    let all = service.explain(None).await;
    assert!(all.outcome.ok);
    assert_eq!(all.answer.unwrap().sources[0].source, "stub");
    let one = service.explain(Some("stub".into())).await;
    assert_eq!(one.answer.unwrap().sources.len(), 1);
    let none = service.explain(Some("nope".into())).await;
    assert_eq!(none.outcome.code.as_deref(), Some("not-found"));
}

#[tokio::test]
async fn search_asks_the_sources_that_cover_the_query_and_pages_with_one_cursor() {
    let (service, stub) = service();
    let q = Query::clause("author", "any", "Abel");
    let first = service.search(q.clone(), None, Some(2), None, None).await;
    let answer = first.answer.expect("ok");
    assert_eq!(answer.rows.len(), 2);
    assert_eq!(answer.rows[0].source, "stub");
    assert!(answer.rows[0].ids.contains_key("doi"), "every row carries ids");
    assert!(answer.diagnostics.iter().any(|d| d.source == "stub" && d.status == DiagnosticStatus::Done));
    let next = answer.next.expect("a next cursor");
    assert!(next.contains("stub"), "the service cursor is a map: {next}");
    let second = service.search(q.clone(), None, Some(2), Some(next), None).await;
    let rows = second.answer.unwrap().rows;
    assert_eq!(rows[0].id, "doi:10.5555/3");
    // A bare cursor is the single source's own.
    let third = service.search(q, Some(vec!["stub".into()]), Some(1), Some("5".into()), None).await;
    assert_eq!(third.answer.unwrap().rows[0].id, "doi:10.5555/5");
    assert_eq!(stub.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_uncovered_index_is_a_diagnostic_never_a_fallback() {
    let (service, stub) = service();
    let q = Query::all(vec![
        Query::clause("author", "any", "Abel"),
        Query::clause("first_author", "any", "Abel"),
    ]);
    // Default selection: the stub does not cover first_author, so it is not asked.
    let r = service.search(q.clone(), None, None, None, None).await;
    let answer = r.answer.unwrap();
    assert!(answer.rows.is_empty());
    assert_eq!(answer.diagnostics[0].status, DiagnosticStatus::Unsupported);
    assert_eq!(answer.diagnostics[0].unsupported.as_deref(), Some("bib.first_author"));
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
    // Named explicitly: it is asked, maps what it can, and says what it dropped.
    let r = service.search(q, Some(vec!["stub".into()]), None, None, None).await;
    let answer = r.answer.unwrap();
    assert_eq!(answer.rows.len(), 3);
    assert!(answer.diagnostics.iter().any(|d| d.unsupported.as_deref() == Some("bib.first_author")));
    assert!(!answer.diagnostics.iter().any(|d| d.status == DiagnosticStatus::Done), "a page with a dropped clause is not `done`");
    let r = service.search(Query::clause("title", "any", "x"), Some(vec!["nope".into()]), None, None, None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("invalid-argument"));
    let r = service.search(Query::clause("title", "any", "x"), Some(vec!["stub".into()]), None, None, Some("cited_by".into())).await;
    assert!(r.answer.unwrap().diagnostics.iter().any(|d| d.status == DiagnosticStatus::Unsupported && d.message.as_deref().unwrap_or("").contains("cited_by")));
}

#[tokio::test]
async fn fetch_merges_the_sources_that_claim_the_id() {
    let a = Stub::new("a");
    let b = Stub::new("b");
    let registry = Registry::new();
    registry.register(a);
    registry.register(b);
    let service = DefaultSourceService::with_registry(registry);
    let r = service.fetch("https://doi.org/10.5555/1".into(), None).await;
    let answer = r.answer.expect("found");
    assert_eq!(answer.work.locations.len(), 2, "one record, two locations");
    assert_eq!(answer.work.title.as_deref(), Some("One from a"));
    assert_eq!(answer.diagnostics.len(), 2);
    let r = service.fetch("10.5555/9".into(), None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("not-found"));
    assert!(r.outcome.message.unwrap().contains("a not-found"));
    let r = service.fetch("10.1/unclaimed".into(), None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("not-found"));
    let r = service.fetch("plain words".into(), None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("invalid-argument"));
}

#[tokio::test]
async fn match_scores_an_identifier_hit_one_and_leaves_a_search_unscored() {
    let (service, _) = service();
    let r = service
        .match_citation(
            MatchInput {
                text: Some("Abel et al. 2002, doi:10.5555/1".into()),
                ..Default::default()
            },
            None,
        )
        .await;
    let answer = r.answer.expect("ok");
    assert!(answer.accepted);
    assert_eq!(answer.rule.as_deref(), Some("identifier"));
    assert_eq!(answer.candidates[0].score, Some(1.0));
    assert_eq!(answer.candidates[0].evidence["identifier"], 1.0);
    assert_eq!(answer.candidates[0].via, "identifier");

    let r = service
        .match_citation(
            MatchInput {
                title: Some("Row 0".into()),
                authors: vec!["Abel".into()],
                ..Default::default()
            },
            None,
        )
        .await;
    let answer = r.answer.expect("ok");
    assert!(!answer.accepted);
    assert_eq!(answer.best, None);
    assert_eq!(answer.candidates[0].score, None);
    assert_eq!(answer.candidates[0].via, "search");

    // A year clause the stub cannot map: no source covers, no candidates, said so.
    let r = service
        .match_citation(
            MatchInput {
                year: Some(2002),
                ..Default::default()
            },
            None,
        )
        .await;
    let answer = r.answer.expect("ok");
    assert!(answer.candidates.is_empty());
}

#[tokio::test]
async fn related_walks_what_the_source_can_and_says_what_it_cannot() {
    let (service, _) = service();
    let r = service.related("doi:10.5555/1".into(), Relation::Cites, None, None).await;
    assert_eq!(r.answer.unwrap().rows[0].id, "doi:10.5555/2");
    let r = service.related("doi:10.5555/1".into(), Relation::Coreads, None, None).await;
    let answer = r.answer.unwrap();
    assert!(answer.rows.is_empty());
    assert_eq!(answer.diagnostics[0].status, DiagnosticStatus::Unsupported);
    let r = service.related("10.1/elsewhere".into(), Relation::Cites, None, None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("not-found"));
}

#[tokio::test]
async fn export_routes_each_id_to_the_source_that_claims_it() {
    let (service, _) = service();
    let r = service
        .export(vec!["10.5555/1".into(), "doi:10.5555/2".into(), "10.1/nobody".into(), "words".into()], ExportFormat::Bibtex)
        .await;
    let answer = r.answer.unwrap();
    assert_eq!(answer.count, 2);
    assert!(answer.text.contains("@article{doi:10.5555/1"));
    assert_eq!(answer.diagnostics.iter().filter(|d| d.status == DiagnosticStatus::NotFound).count(), 2);
    let r = service.export(vec!["10.5555/1".into()], ExportFormat::CslJson).await;
    let text = r.answer.unwrap().text;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["DOI"], json!("10.5555/1"));
    assert_eq!(v[0]["custom"]["ids"]["doi"], json!("10.5555/1"));
}
