//! Tier A: every verb is found in the linked inventory and run through the
//! pipeline (docs/capability.md § 8); the examples are offline by
//! construction (an uncovered index, a non-identifier, an empty list). The
//! adapter-level behaviour is pinned with a stub source that never leaves
//! the process.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use impress_service_core::call;
use impress_service_core::descriptor_handle::VerbHandle;
use serde_json::{json, Value};

use koine_sources::explain::{Etiquette, IndexDef, Page as PageDecl, PageModel};
use koine_sources::kind::{Location, Version};
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};

use crate::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use crate::dto::MatchInput;
use crate::etiquette::{Gate, Limiter};
use crate::registry::Registry;
use crate::service::{BibSourceService, DefaultBibSourceService};

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
            ty: "article-journal".into(),
            title: Some(title.into()),
            doi: Some(doi.into()),
            ids: BTreeMap::from([("doi".to_string(), doi.to_string())]),
            locations: vec![Location {
                source: self.source.into(),
                source_id: doi.into(),
                version: Version::Published,
                url: None,
                pdf: None,
                license: None,
                oa_status: None,
                primary: true,
            }],
            provenance: Some(koine_sources::kind::Provenance {
                fetched_at: "2026-10-19T00:00:00Z".into(),
                source: self.source.into(),
                source_id: doi.into(),
                query: None,
                sources: Vec::new(),
            }),
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
                    IndexDef {
                        relations: vec!["any".into()],
                        exact: None,
                    },
                ),
                (
                    "title".into(),
                    IndexDef {
                        relations: vec!["any".into()],
                        exact: None,
                    },
                ),
                (
                    "doi".into(),
                    IndexDef {
                        relations: vec!["=".into()],
                        exact: Some(true),
                    },
                ),
            ]),
            sortable: vec!["relevance".into(), "year".into()],
            identifiers: vec!["doi".into()],
            fills: vec!["title".into(), "doi".into()],
            page: PageDecl {
                model: PageModel::Cursor,
                max: Some(50),
            },
            etiquette: Etiquette::default(),
            secrets: vec![],
            native: None,
        }
    }

    fn detect(&self, id: &str) -> bool {
        id.starts_with("doi:10.5555/")
    }

    async fn search(&self, query: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut out = Page {
            diagnostics: query.covered_by(&self.explain()),
            ..Default::default()
        };
        let start: usize = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        for i in start..(start + page.limit as usize).min(start + 3) {
            out.rows
                .push(self.work(&format!("10.5555/{i}"), &format!("Row {i}")));
        }
        out.next = (start + 3 < 6).then(|| (start + 3).to_string());
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let doi = id.trim_start_matches("doi:");
        Ok((doi == "10.5555/1").then(|| self.work(doi, &format!("One from {}", self.source))))
    }

    async fn related(
        &self,
        _id: &str,
        relation: Relation,
        _page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        match relation {
            Relation::Cites => Ok(Page {
                rows: vec![self.work("10.5555/2", "Two")],
                ..Default::default()
            }),
            other => Err(Diagnostic::unsupported(self.source, other.as_str())),
        }
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        Ok(match format {
            ExportFormat::CslJson => serde_json::to_string(
                &ids.iter()
                    .map(|id| self.work(id.trim_start_matches("doi:"), "X").to_csl_json())
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

fn service() -> (DefaultBibSourceService, Arc<Stub>) {
    let stub = Stub::new("stub");
    let registry = Registry::new();
    registry.register(stub.clone());
    (DefaultBibSourceService::with_registry(registry), stub)
}

fn contains(haystack: &Value, needle: &Value) -> bool {
    match (haystack, needle) {
        (Value::Object(h), Value::Object(n)) => n
            .iter()
            .all(|(k, v)| h.get(k).is_some_and(|hv| contains(hv, v))),
        (Value::Array(h), Value::Array(n)) => {
            h.len() == n.len() && h.iter().zip(n).all(|(a, b)| contains(a, b))
        }
        _ => haystack == needle,
    }
}

#[test]
fn every_verb_is_documented_has_an_example_and_the_example_answers_offline() {
    let verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == "bib-source-service")
        .collect();
    let mut names: Vec<&str> = verbs.iter().map(|v| v.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "bib-source-service_explain",
            "bib-source-service_export",
            "bib-source-service_fetch",
            "bib-source-service_match-citation",
            "bib-source-service_related",
            "bib-source-service_search",
        ]
    );
    for verb in &verbs {
        assert!(!verb.description().is_empty(), "{}", verb.name());
        assert!(
            !verb.examples().is_empty(),
            "{} has no example",
            verb.name()
        );
        assert!(
            verb.effects()
                .reach
                .iter()
                .any(|r| matches!(r, impress_service_core::descriptor::Reach::Network)),
            "{} must declare the network reach",
            verb.name()
        );
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value())
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            let expect: Value =
                serde_json::from_str(example.expect.expect("every example states its expect"))
                    .unwrap();
            assert!(contains(&result, &expect), "{label}: {result} vs {expect}");
        }
    }
}

#[test]
fn a_misspelt_argument_is_refused_before_the_handler() {
    let result = call::call("bib-source-service_search", json!({"querry": {}})).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
}

#[test]
fn a_refused_search_carries_only_the_outcome() {
    let result = call::call("bib-source-service_search", json!({"query": {"and": []}})).unwrap();
    assert_eq!(result["ok"], json!(false), "{result}");
    assert!(result.get("rows").is_none(), "{result}");
    let result = call::call(
        "bib-source-service_search",
        json!({"query": {"index": "title", "relation": "any", "term": "x"}, "limit": 101}),
    )
    .unwrap();
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
    let first = service
        .search(q.clone(), None, Some(2), None, None, None)
        .await;
    let answer = first.answer.expect("ok");
    assert_eq!(answer.rows.len(), 2);
    assert_eq!(answer.rows[0].locations[0].source, "stub");
    assert!(
        answer.rows[0].ids.contains_key("doi"),
        "every row carries ids"
    );
    assert_eq!(answer.rows[0].provenance.as_ref().unwrap().source, "stub");
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.source == "stub" && d.status == Status::Done));
    let next = answer.next.expect("a next cursor");
    assert!(next.contains("stub"), "the service cursor is a map: {next}");
    let second = service
        .search(q.clone(), None, Some(2), Some(next), None, None)
        .await;
    let rows = second.answer.unwrap().rows;
    assert_eq!(rows[0].doi.as_deref(), Some("10.5555/3"));
    // A bare cursor is the single source's own.
    let third = service
        .search(
            q,
            Some(vec!["stub".into()]),
            Some(1),
            Some("5".into()),
            None,
            None,
        )
        .await;
    assert_eq!(
        third.answer.unwrap().rows[0].doi.as_deref(),
        Some("10.5555/5")
    );
    assert_eq!(stub.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_uncovered_index_is_a_diagnostic_never_a_fallback() {
    let (service, stub) = service();
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel"),
            Query::clause("first_author", "any", "Abel"),
        ],
    };
    // Default selection: the stub does not cover first_author, so it is not asked.
    let r = service
        .search(q.clone(), None, None, None, None, None)
        .await;
    let answer = r.answer.unwrap();
    assert!(answer.rows.is_empty());
    assert_eq!(answer.diagnostics[0].status, Status::Unsupported);
    assert_eq!(answer.diagnostics[0].detail, "bib.first_author");
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
    // Named explicitly: it is asked, maps what it can, and says what it dropped.
    let r = service
        .search(q, Some(vec!["stub".into()]), None, None, None, None)
        .await;
    let answer = r.answer.unwrap();
    assert_eq!(answer.rows.len(), 3);
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.detail == "bib.first_author"));
    assert!(
        !answer.diagnostics.iter().any(|d| d.status == Status::Done),
        "a page with a dropped clause is not `done`"
    );
    let r = service
        .search(
            Query::clause("title", "any", "x"),
            Some(vec!["nope".into()]),
            None,
            None,
            None,
            None,
        )
        .await;
    assert_eq!(r.outcome.code.as_deref(), Some("invalid-argument"));
    let r = service
        .search(
            Query::clause("title", "any", "x"),
            Some(vec!["stub".into()]),
            None,
            None,
            Some("cited_by".into()),
            None,
        )
        .await;
    assert!(r
        .answer
        .unwrap()
        .diagnostics
        .iter()
        .any(|d| d.status == Status::Unsupported && d.detail.contains("cited_by")));
}

#[tokio::test]
async fn fetch_merges_the_sources_that_claim_the_id() {
    let a = Stub::new("a");
    let b = Stub::new("b");
    let registry = Registry::new();
    registry.register(a);
    registry.register(b);
    let service = DefaultBibSourceService::with_registry(registry);
    let r = service
        .fetch("https://doi.org/10.5555/1".into(), None, None)
        .await;
    let answer = r.answer.expect("found");
    assert_eq!(answer.work.locations.len(), 2, "one record, two locations");
    answer.work.validate().unwrap();
    assert_eq!(answer.work.title.as_deref(), Some("One from a"));
    assert_eq!(answer.diagnostics.len(), 2);
    let r = service.fetch("10.5555/9".into(), None, None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("not-found"));
    assert!(r.outcome.message.unwrap().contains("a not-found"));
    let r = service.fetch("10.1/unclaimed".into(), None, None).await;
    assert_eq!(r.outcome.code.as_deref(), Some("not-found"));
    let r = service.fetch("plain words".into(), None, None).await;
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
    let r = service
        .related("doi:10.5555/1".into(), Relation::Cites, None, None, None)
        .await;
    assert_eq!(r.answer.unwrap().rows[0].doi.as_deref(), Some("10.5555/2"));
    let r = service
        .related("doi:10.5555/1".into(), Relation::Coreads, None, None, None)
        .await;
    let answer = r.answer.unwrap();
    assert!(answer.rows.is_empty());
    assert_eq!(answer.diagnostics[0].status, Status::Unsupported);
    let r = service
        .related("10.1/elsewhere".into(), Relation::Cites, None, None, None)
        .await;
    assert_eq!(r.outcome.code.as_deref(), Some("not-found"));
}

#[tokio::test]
async fn export_routes_each_id_to_the_source_that_claims_it() {
    let (service, _) = service();
    let r = service
        .export(
            vec![
                "10.5555/1".into(),
                "doi:10.5555/2".into(),
                "10.1/nobody".into(),
                "words".into(),
            ],
            ExportFormat::Bibtex,
        )
        .await;
    let answer = r.answer.unwrap();
    assert_eq!(answer.count, 2);
    assert!(answer.text.contains("@article{doi:10.5555/1"));
    assert_eq!(
        answer
            .diagnostics
            .iter()
            .filter(|d| d.source == "bib-source-service" && d.status == Status::Unsupported)
            .count(),
        2
    );
    let r = service
        .export(vec!["10.5555/1".into()], ExportFormat::CslJson)
        .await;
    let text = r.answer.unwrap().text;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["DOI"], json!("10.5555/1"));
    assert_eq!(v[0]["custom"]["ids"]["doi"], json!("10.5555/1"));
}

// ── S-D7 and S-D8 pins: federation and etiquette with stub sources ──────────

/// A source scripted for the federation tests: a fixed page, or a sleep, or
/// a panic, or a 429 that backs the source off through the gate's limiter —
/// the way Crossref's adapter does.
struct Scripted {
    source: &'static str,
    rows: Vec<Work>,
    sleep: Option<Duration>,
    panic: bool,
    /// Answer 429 with this Retry-After, recording it on `limiter`.
    rate_limit: Option<(Duration, Arc<Limiter>)>,
    contact: Option<&'static str>,
    per_day: Option<u64>,
    calls: AtomicUsize,
}

impl Scripted {
    fn new(source: &'static str, rows: Vec<Work>) -> Self {
        Scripted {
            source,
            rows,
            sleep: None,
            panic: false,
            rate_limit: None,
            contact: None,
            per_day: None,
            calls: AtomicUsize::new(0),
        }
    }
}

/// A work from `source` with the given ids (`[("doi", "10.1/x")]`), title,
/// year and citation count; the location's url names the source.
fn scripted_work(source: &str, ids: &[(&str, &str)], title: &str, year: i32, cited: u64) -> Work {
    let source_id = ids.first().map_or("none", |(_, v)| *v).to_string();
    Work {
        ty: "article-journal".into(),
        title: Some(title.into()),
        issued: Some(koine_sources::kind::Issued {
            date_parts: vec![vec![year]],
        }),
        ids: ids
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        locations: vec![Location {
            source: source.into(),
            source_id: source_id.clone(),
            version: Version::Published,
            url: Some(format!("https://{source}.example/{source_id}")),
            pdf: None,
            license: None,
            oa_status: None,
            primary: true,
        }],
        provenance: Some(koine_sources::kind::Provenance {
            fetched_at: "2026-10-19T00:00:00Z".into(),
            source: source.into(),
            source_id,
            query: None,
            sources: vec![],
        }),
        metrics: Some(koine_sources::kind::Metrics {
            cited_by: Some(koine_sources::kind::Counted {
                value: cited,
                source: source.into(),
            }),
            cited_by_all: vec![],
        }),
        ..Default::default()
    }
}

#[async_trait]
impl Adapter for Scripted {
    fn explain(&self) -> ExplainRecord {
        let mut record = Stub::new(self.source).explain();
        record.etiquette = Etiquette {
            contact: self.contact.map(String::from),
            limit: None,
            key: None,
            budget: self.per_day.map(|per_day| koine_sources::explain::Budget {
                per_day,
                remaining: None,
                resets_at: None,
            }),
        };
        record
    }

    fn detect(&self, id: &str) -> bool {
        id.starts_with("doi:10.1/")
    }

    async fn search(&self, _query: &Query, _page: PageRequest) -> Result<Page, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((retry_after, limiter)) = &self.rate_limit {
            limiter.back_off(self.source, Some(*retry_after));
            return Err(Diagnostic::new(
                self.source,
                Status::RateLimited,
                format!(
                    "429 from {}; backing off {}s, not retrying",
                    self.source,
                    retry_after.as_secs()
                ),
            ));
        }
        if self.panic {
            panic!("scripted panic in {}", self.source);
        }
        if let Some(d) = self.sleep {
            tokio::time::sleep(d).await;
        }
        Ok(Page {
            rows: self.rows.clone(),
            ..Default::default()
        })
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(d) = self.sleep {
            tokio::time::sleep(d).await;
        }
        let doi = id.trim_start_matches("doi:");
        Ok(self
            .rows
            .iter()
            .find(|w| {
                w.ids
                    .get("doi")
                    .is_some_and(|d| d.eq_ignore_ascii_case(doi))
            })
            .cloned())
    }

    async fn related(
        &self,
        _id: &str,
        _relation: Relation,
        _page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Page {
            rows: self.rows.clone(),
            ..Default::default()
        })
    }

    async fn export(&self, _ids: &[String], _format: ExportFormat) -> Result<String, Diagnostic> {
        Ok(String::new())
    }
}

fn federated(adapters: Vec<Arc<Scripted>>, gate: Gate) -> DefaultBibSourceService {
    let registry = Registry::new();
    for a in adapters {
        registry.register(a);
    }
    DefaultBibSourceService::with_registry(registry).with_gate(Arc::new(gate))
}

fn any_query() -> Query {
    Query::clause("title", "any", "stars")
}

fn statuses(diagnostics: &[Diagnostic], source: &str) -> Vec<Status> {
    diagnostics
        .iter()
        .filter(|d| d.source == source)
        .map(|d| d.status)
        .collect()
}

#[tokio::test]
async fn rows_sharing_a_doi_merge_to_one_with_the_priority_body_and_both_locations() {
    // openalex (priority 50) is listed first; crossref (10) still supplies the body.
    let openalex = Arc::new(Scripted::new(
        "openalex",
        vec![scripted_work(
            "openalex",
            &[("doi", "10.1/ABC"), ("openalex", "W1")],
            "Title from OpenAlex",
            2020,
            9,
        )],
    ));
    let mut crossref_row = scripted_work(
        "crossref",
        &[("doi", "10.1/abc")],
        "Title from Crossref",
        2020,
        4,
    );
    crossref_row.container_title = Some("Science".into());
    crossref_row.abstract_ = None;
    let crossref = Arc::new(Scripted::new("crossref", vec![crossref_row]));
    let service = federated(vec![openalex, crossref], Gate::new());
    let r = service
        .search(any_query(), None, None, None, None, None)
        .await;
    let answer = r.answer.expect("ok");
    assert_eq!(answer.rows.len(), 1, "one row: {:?}", answer.rows);
    let row = &answer.rows[0];
    assert_eq!(row.title.as_deref(), Some("Title from Crossref"));
    assert_eq!(row.container_title.as_deref(), Some("Science"));
    assert_eq!(row.ids["openalex"], "W1", "ids are the union");
    assert_eq!(
        row.locations.len(),
        2,
        "both locations: {:?}",
        row.locations
    );
    assert_eq!(row.locations.iter().filter(|l| l.primary).count(), 1);
    let metrics = row.metrics.as_ref().unwrap();
    assert_eq!(metrics.cited_by.as_ref().unwrap().source, "crossref");
    assert_eq!(
        metrics.cited_by_all.len(),
        2,
        "every source's count is kept"
    );
    assert_eq!(
        row.provenance.as_ref().unwrap().sources,
        vec!["crossref".to_string(), "openalex".to_string()]
    );
    assert_eq!(statuses(&answer.diagnostics, "openalex"), [Status::Done]);
    assert_eq!(statuses(&answer.diagnostics, "crossref"), [Status::Done]);
    row.validate().unwrap();
}

#[tokio::test]
async fn an_arxiv_version_and_a_doi_case_do_not_split_a_row() {
    let a = Arc::new(Scripted::new(
        "arxiv",
        vec![scripted_work(
            "arxiv",
            &[("arxiv", "2101.00001v2")],
            "Preprint",
            2021,
            0,
        )],
    ));
    let b = Arc::new(Scripted::new(
        "openalex",
        vec![scripted_work(
            "openalex",
            &[("arxiv", "2101.00001"), ("doi", "10.1/p")],
            "Published",
            2022,
            3,
        )],
    ));
    let service = federated(vec![a, b], Gate::new());
    let answer = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert_eq!(answer.rows.len(), 1);
    assert_eq!(
        answer.rows[0].title.as_deref(),
        Some("Published"),
        "openalex (50) outranks arxiv (60)"
    );
}

#[tokio::test]
async fn the_same_title_with_no_shared_id_stays_two_rows() {
    let a = Arc::new(Scripted::new(
        "crossref",
        vec![scripted_work(
            "crossref",
            &[("doi", "10.1/one")],
            "The Same Title",
            2020,
            1,
        )],
    ));
    let b = Arc::new(Scripted::new(
        "openalex",
        vec![scripted_work(
            "openalex",
            &[("openalex", "W2")],
            "The Same Title",
            2020,
            1,
        )],
    ));
    let service = federated(vec![a, b], Gate::new());
    let answer = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert_eq!(answer.rows.len(), 2, "a title alone never merges");
}

#[tokio::test]
async fn a_slow_source_times_out_and_the_others_rows_return() {
    let mut slow = Scripted::new(
        "wos",
        vec![scripted_work(
            "wos",
            &[("doi", "10.1/slow")],
            "Slow",
            2020,
            0,
        )],
    );
    slow.sleep = Some(Duration::from_secs(5));
    let slow = Arc::new(slow);
    let quick = Arc::new(Scripted::new(
        "crossref",
        vec![scripted_work(
            "crossref",
            &[("doi", "10.1/quick")],
            "Quick",
            2020,
            0,
        )],
    ));
    let service = federated(vec![slow.clone(), quick], Gate::new());
    let started = Instant::now();
    let answer = service
        .search(any_query(), None, None, None, None, Some(100))
        .await
        .answer
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "did not wait for the slow one"
    );
    assert_eq!(answer.rows.len(), 1);
    assert_eq!(answer.rows[0].title.as_deref(), Some("Quick"));
    let timed_out = answer
        .diagnostics
        .iter()
        .find(|d| d.source == "wos")
        .expect("wos has an outcome");
    assert_eq!(timed_out.status, Status::TimedOut);
    assert!(timed_out.detail.contains("100ms"), "{timed_out:?}");
    assert_eq!(statuses(&answer.diagnostics, "crossref"), [Status::Done]);
    assert_eq!(slow.calls.load(Ordering::SeqCst), 1);
    // A zero timeout is refused, not a storm of timed_out.
    let r = service
        .search(any_query(), None, None, None, None, Some(0))
        .await;
    assert_eq!(r.outcome.code.as_deref(), Some("invalid-argument"));
}

#[tokio::test]
async fn a_panicking_source_is_its_own_error_never_the_calls() {
    let mut bad = Scripted::new("pubmed", vec![]);
    bad.panic = true;
    let good = Arc::new(Scripted::new(
        "crossref",
        vec![scripted_work(
            "crossref",
            &[("doi", "10.1/fine")],
            "Fine",
            2020,
            0,
        )],
    ));
    let service = federated(vec![Arc::new(bad), good], Gate::new());
    let r = service
        .search(any_query(), None, None, None, None, None)
        .await;
    let answer = r.answer.expect("the call succeeds");
    assert_eq!(answer.rows.len(), 1);
    let error = answer
        .diagnostics
        .iter()
        .find(|d| d.source == "pubmed")
        .unwrap();
    assert_eq!(error.status, Status::Error);
    assert!(error.detail.contains("panicked"), "{error:?}");
}

#[tokio::test]
async fn sort_by_year_runs_across_sources_and_no_sort_keeps_the_first_sources_order() {
    let arxiv = Arc::new(Scripted::new(
        "arxiv",
        vec![
            scripted_work("arxiv", &[("arxiv", "1")], "A 2019", 2019, 0),
            scripted_work("arxiv", &[("arxiv", "2")], "A 2024", 2024, 0),
        ],
    ));
    let crossref = Arc::new(Scripted::new(
        "crossref",
        vec![
            scripted_work("crossref", &[("doi", "10.1/c1")], "C 2021", 2021, 0),
            scripted_work("crossref", &[("doi", "10.1/c2")], "C 2023", 2023, 0),
        ],
    ));
    let service = federated(vec![arxiv, crossref], Gate::new());
    let titles = |answer: &crate::dto::RowsAnswer| -> Vec<String> {
        answer
            .rows
            .iter()
            .map(|r| r.title.clone().unwrap())
            .collect()
    };
    let by_year = service
        .search(any_query(), None, None, None, Some("year".into()), None)
        .await
        .answer
        .unwrap();
    assert_eq!(titles(&by_year), ["A 2024", "C 2023", "C 2021", "A 2019"]);
    // No sort: arxiv was listed first, so its order leads; crossref follows.
    let plain = service
        .search(
            any_query(),
            Some(vec!["arxiv".into(), "crossref".into()]),
            None,
            None,
            None,
            None,
        )
        .await
        .answer
        .unwrap();
    assert_eq!(titles(&plain), ["A 2019", "A 2024", "C 2021", "C 2023"]);
    // Listed the other way round, crossref leads.
    let reversed = service
        .search(
            any_query(),
            Some(vec!["crossref".into(), "arxiv".into()]),
            None,
            None,
            None,
            None,
        )
        .await
        .answer
        .unwrap();
    assert_eq!(titles(&reversed), ["C 2021", "C 2023", "A 2019", "A 2024"]);
}

#[tokio::test]
async fn a_429_is_rate_limited_once_and_the_next_call_in_the_window_sends_nothing() {
    let gate = Gate::new();
    let mut limited = Scripted::new("openalex", vec![]);
    limited.rate_limit = Some((Duration::from_secs(60), gate.limiter()));
    let limited = Arc::new(limited);
    let other = Arc::new(Scripted::new(
        "crossref",
        vec![scripted_work(
            "crossref",
            &[("doi", "10.1/ok")],
            "Ok",
            2020,
            0,
        )],
    ));
    let service = federated(vec![limited.clone(), other], gate);
    let first = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert_eq!(
        statuses(&first.diagnostics, "openalex"),
        [Status::RateLimited]
    );
    assert_eq!(first.rows.len(), 1, "the other source still answers");
    assert_eq!(
        limited.calls.load(Ordering::SeqCst),
        1,
        "hit exactly once, no retry"
    );
    let second = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    let d = second
        .diagnostics
        .iter()
        .find(|d| d.source == "openalex")
        .unwrap();
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("nothing was sent"), "{d:?}");
    assert_eq!(
        limited.calls.load(Ordering::SeqCst),
        1,
        "the window sends nothing"
    );
}

#[tokio::test]
async fn a_spent_daily_budget_skips_the_source_and_explain_shows_zero_remaining() {
    let mut metered = Scripted::new(
        "ads",
        vec![scripted_work(
            "ads",
            &[("bibcode", "2020ApJ...1A")],
            "Metered",
            2020,
            0,
        )],
    );
    metered.per_day = Some(2);
    let metered = Arc::new(metered);
    let service = federated(vec![metered.clone()], Gate::new());
    let budget =
        |answer: crate::dto::ExplainAnswer| answer.sources[0].etiquette.budget.clone().unwrap();
    let before = budget(service.explain(None).await.answer.unwrap());
    assert_eq!((before.per_day, before.remaining), (2, Some(2)));
    for _ in 0..2 {
        let answer = service
            .search(any_query(), None, None, None, None, None)
            .await
            .answer
            .unwrap();
        assert_eq!(answer.rows.len(), 1);
    }
    let third = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert!(third.rows.is_empty());
    let d = &third.diagnostics[0];
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("daily budget of 2 spent"), "{d:?}");
    assert_eq!(
        metered.calls.load(Ordering::SeqCst),
        2,
        "the third call sent nothing"
    );
    let explained = service.explain(None).await.answer.unwrap();
    assert_eq!(explained.federation.timeout_ms, 8000);
    assert_eq!(explained.federation.merge, "identifier");
    let after = budget(explained);
    assert_eq!(after.remaining, Some(0));
    assert!(after.resets_at.unwrap().ends_with("T00:00:00Z"));
}

#[tokio::test]
async fn the_missing_contact_warning_fires_once_per_polite_pool_source() {
    let mut polite = Scripted::new(
        "crossref",
        vec![scripted_work(
            "crossref",
            &[("doi", "10.1/p")],
            "Polite",
            2020,
            0,
        )],
    );
    polite.contact = Some("mailto");
    let polite = Arc::new(polite);
    let mut blunt = Scripted::new("arxiv", vec![]);
    blunt.contact = Some("user-agent");
    let service = federated(
        vec![polite, Arc::new(blunt)],
        Gate::new().with_contact(None),
    );
    let warning = |answer: &crate::dto::RowsAnswer| {
        answer
            .diagnostics
            .iter()
            .filter(|d| d.detail == crate::etiquette::NO_CONTACT_DETAIL)
            .map(|d| d.source.clone())
            .collect::<Vec<_>>()
    };
    let first = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert_eq!(warning(&first), ["crossref"], "{:?}", first.diagnostics);
    assert_eq!(first.rows.len(), 1, "the warning is `done`, not a refusal");
    let second = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert!(
        warning(&second).is_empty(),
        "warned once: {:?}",
        second.diagnostics
    );
    // With a contact, nothing to warn about.
    let service = federated(
        vec![Arc::new(Scripted::new("crossref", vec![]))],
        Gate::new().with_contact(Some("mailto:someone@example.org".into())),
    );
    let answer = service
        .search(any_query(), None, None, None, None, None)
        .await
        .answer
        .unwrap();
    assert!(warning(&answer).is_empty());
}

#[tokio::test]
async fn fetch_is_federated_too_a_slow_claimant_times_out() {
    let mut slow = Scripted::new(
        "wos",
        vec![scripted_work(
            "wos",
            &[("doi", "10.1/f")],
            "From WoS",
            2020,
            0,
        )],
    );
    slow.sleep = Some(Duration::from_secs(5));
    let quick = Arc::new(Scripted::new(
        "crossref",
        vec![scripted_work(
            "crossref",
            &[("doi", "10.1/f")],
            "From Crossref",
            2020,
            2,
        )],
    ));
    let service = federated(vec![Arc::new(slow), quick], Gate::new());
    let answer = service
        .fetch("10.1/F".into(), None, Some(100))
        .await
        .answer
        .expect("found");
    assert_eq!(answer.work.title.as_deref(), Some("From Crossref"));
    assert_eq!(statuses(&answer.diagnostics, "wos"), [Status::TimedOut]);
    assert_eq!(statuses(&answer.diagnostics, "crossref"), [Status::Done]);
}
