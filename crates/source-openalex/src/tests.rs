//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches api.openalex.org
//! except the one `#[ignore]` live test at the end.

use std::sync::Arc;
use std::time::Duration;

use koine_sources::kind::Version;
use koine_sources::{Query, Status};
use mockito::{Matcher, Server, ServerGuard};
use serde_json::{json, Value};
use source_service::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use source_service::etiquette::Limiter;
use source_service::service::{DefaultSourceService, SourceService};
use source_service::Registry;

use crate::{OpenAlex, SOURCE};

const DOI: &str = "10.1126/science.1063991";
const W: &str = "W2040912209";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn headers(name: &str) -> Vec<(String, String)> {
    fixture(name)
        .lines()
        .filter_map(|l| {
            l.split_once(": ")
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .filter(|(k, _)| k.starts_with("x-ratelimit") || k == "retry-after" || k == "content-type")
        .collect()
}

fn with_headers(m: mockito::Mock, file: &str) -> mockito::Mock {
    let mut m = m;
    for (k, v) in headers(file) {
        m = m.with_header(&k, &v);
    }
    m
}

async fn adapter() -> (ServerGuard, OpenAlex) {
    let server = Server::new_async().await;
    let a = OpenAlex::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    (server, a)
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort: None,
    }
}

#[test]
fn the_explain_record_is_valid_and_honest_about_what_it_lacks() {
    let record = crate::explain_json();
    assert_eq!(record.source, "openalex");
    assert_eq!(record.page.max, Some(100));
    assert!(record.secrets.is_empty());
    assert_eq!(record.etiquette.contact.as_deref(), Some("mailto"));
    for i in [
        "any", "title", "abstract", "author", "year", "doi", "pmid", "orcid", "cites", "cited_by",
    ] {
        assert!(record.indexes.contains_key(i), "{i}");
    }
    // No works filter takes a journal's name.
    assert!(!record.indexes.contains_key("container"));
    let a = OpenAlex::new();
    for yes in [
        "openalex:W2040912209",
        "doi:10.1126/science.1063991",
        "pmid:11711636",
    ] {
        assert!(a.detect(yes), "{yes}");
    }
    for no in [
        "openalex:2040912209",
        "arxiv:2101.00001",
        "bibcode:2002Sci...295...93A",
        "doi:nope",
    ] {
        assert!(!a.detect(no), "{no}");
    }
}

#[tokio::test]
async fn a_search_sends_filters_cursor_per_page_sort_and_mailto_and_reads_the_page() {
    let (mut server, oa) = adapter().await;
    let m = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded(
                "filter".into(),
                "raw_author_name.search:Abel,title.search:first star".into(),
            ),
            Matcher::UrlEncoded("per-page".into(), "3".into()),
            Matcher::UrlEncoded("cursor".into(), "*".into()),
            Matcher::UrlEncoded("sort".into(), "cited_by_count:desc".into()),
            Matcher::UrlEncoded("mailto".into(), "tabel@stanford.edu".into()),
        ]))
        .match_header(
            "user-agent",
            Matcher::Regex("koine-source-openalex/.*mailto:tabel@stanford.edu".into()),
        )
        .with_status(200)
        .with_body(fixture("search.json"));
    let m = with_headers(m, "search.headers")
        .expect(1)
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel"),
            Query::clause("title", "any", "first star"),
        ],
    };
    let p = oa.search(&q, page(3)).await.unwrap();
    m.assert_async().await;
    assert_eq!(p.rows.len(), 3);
    assert!(p.diagnostics.is_empty());
    assert_eq!(
        p.next.as_deref(),
        Some("Ils3Ni4wODE3NSwgJy1JbmZpbml0eScsIDIsICdodHRwczovL29wZW5hbGV4Lm9yZy9XMzAwNzYyNjE3OSddIg==")
    );
    let row = &p.rows[0];
    assert_eq!(row.ids["openalex"], W);
    assert_eq!(row.provenance.as_ref().unwrap().source_id, W);
    assert!(row
        .provenance
        .as_ref()
        .unwrap()
        .query
        .as_deref()
        .unwrap()
        .contains("first star"));
    for r in &p.rows {
        r.validate().unwrap();
    }
}

#[tokio::test]
async fn any_is_search_and_relevance_is_the_sort_and_a_short_page_has_no_next() {
    let (mut server, oa) = adapter().await;
    let m = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("search".into(), "first star".into()),
            Matcher::UrlEncoded("filter".into(), "publication_year:2000-2003".into()),
            Matcher::UrlEncoded("sort".into(), "relevance_score:desc".into()),
            Matcher::UrlEncoded("per-page".into(), "100".into()),
        ]))
        .with_status(200)
        .with_body(fixture("search.json")) // 3 rows of the 100 asked
        .expect(1)
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("any", "any", "first star"),
            Query::clause("year", "within", "2000 2003"),
        ],
    };
    let p = oa.search(&q, page(5000)).await.unwrap();
    m.assert_async().await;
    assert_eq!(p.next, None, "a short page is the end");
}

#[tokio::test]
async fn a_fetch_by_doi_parses_every_field_of_the_work_kind() {
    let (mut server, oa) = adapter().await;
    let m = server
        .mock("GET", "/works/https://doi.org/10.1126/science.1063991")
        .match_query(Matcher::UrlEncoded(
            "mailto".into(),
            "tabel@stanford.edu".into(),
        ))
        .with_status(200)
        .with_body(fixture("fetch.json"));
    let m = with_headers(m, "fetch.headers").create_async().await;
    let w = oa
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("found");
    m.assert_async().await;
    assert_eq!(w.ty, "article-journal");
    assert_eq!(
        w.title.as_deref(),
        Some("The Formation of the First Star in the Universe")
    );
    assert_eq!(w.author.len(), 3);
    assert_eq!(w.author[0].family.as_deref(), Some("Abel"));
    assert_eq!(w.author[0].given.as_deref(), Some("Tom"));
    assert_eq!(w.author[0].orcid.as_deref(), Some("0000-0002-5969-1251"));
    assert_eq!(
        w.issued.as_ref().unwrap().date_parts,
        vec![vec![2002, 1, 4]]
    );
    assert_eq!(w.container_title.as_deref(), Some("Science"));
    assert_eq!(w.volume.as_deref(), Some("295"));
    assert_eq!(w.issue.as_deref(), Some("5552"));
    assert_eq!(w.page.as_deref(), Some("93-98"));
    assert_eq!(w.language.as_deref(), Some("en"));
    assert!(
        w.abstract_
            .as_deref()
            .unwrap()
            .starts_with("We describe results from a fully"),
        "{:?}",
        w.abstract_
    );
    assert_eq!(w.doi.as_deref(), Some(DOI));
    assert_eq!(w.ids["doi"], DOI);
    assert_eq!(w.ids["openalex"], W);
    assert_eq!(w.ids["pmid"], "11711636");
    assert_eq!(w.ids["mag"], "2040912209");
    assert_eq!(
        w.ids["arxiv"], "astro-ph/0112088",
        "from the arxiv.org location"
    );
    let cited = w.metrics.as_ref().unwrap().cited_by.as_ref().unwrap();
    assert_eq!((cited.value, cited.source.as_str()), (1447, SOURCE));
    // Locations: the publisher (primary, published, closed), the PubMed and
    // CERN repository records, the arXiv e-print (open, with a pdf), and
    // arXiv's own DOI landing page.
    assert_eq!(w.locations.len(), 5);
    let primary = &w.locations[0];
    assert!(primary.primary);
    assert_eq!(primary.version, Version::Published);
    assert_eq!(primary.oa_status, None, "is_oa false");
    let arxiv = w
        .locations
        .iter()
        .find(|l| l.url.as_deref() == Some("http://arxiv.org/abs/astro-ph/0112088"))
        .unwrap();
    assert_eq!(arxiv.version, Version::Submitted);
    assert_eq!(
        arxiv.pdf.as_deref(),
        Some("https://arxiv.org/pdf/astro-ph/0112088")
    );
    assert_eq!(
        arxiv.oa_status.as_deref(),
        Some("green"),
        "the best_oa_location"
    );
    assert!(!arxiv.primary);
    assert_eq!(w.locations.iter().filter(|l| l.primary).count(), 1);
    w.validate().unwrap();
    let csl = w.to_csl_json();
    assert_eq!(csl["custom"]["ids"]["openalex"], json!(W));
}

#[test]
fn abstracts_are_rebuilt_from_the_inverted_index_and_versions_map_to_driver() {
    let v: Value = json!({"b": [1], "a": [0, 3], "c": [2]});
    assert_eq!(
        crate::parse::abstract_of(v.as_object().unwrap()).as_deref(),
        Some("a b c a")
    );
    assert_eq!(crate::parse::abstract_of(&serde_json::Map::new()), None);
    use crate::parse::version_of;
    assert_eq!(
        version_of(&json!({"version": "publishedVersion"})),
        Version::Published
    );
    assert_eq!(
        version_of(&json!({"version": "acceptedVersion"})),
        Version::Accepted
    );
    assert_eq!(
        version_of(&json!({"version": "submittedVersion"})),
        Version::Submitted
    );
    assert_eq!(
        version_of(&json!({"version": null, "is_published": true})),
        Version::Published
    );
    assert_eq!(version_of(&json!({"is_accepted": true})), Version::Accepted);
    assert_eq!(version_of(&json!({})), Version::Submitted);
}

#[tokio::test]
async fn a_fetch_miss_is_none_and_a_foreign_id_is_not_ours() {
    let (mut server, oa) = adapter().await;
    let _m = server
        .mock("GET", "/works/W1")
        .match_query(Matcher::Any)
        .with_status(404)
        .with_body(fixture("notfound.json"))
        .create_async()
        .await;
    assert_eq!(oa.fetch("openalex:W1").await.unwrap(), None);
    assert_eq!(oa.fetch("arxiv:2101.00001").await.unwrap(), None);
}

#[tokio::test]
async fn a_lone_doi_clause_is_a_lookup() {
    let (mut server, oa) = adapter().await;
    let _m = server
        .mock("GET", "/works/https://doi.org/10.1126/science.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let p = oa
        .search(&Query::clause("doi", "=", DOI), page(5))
        .await
        .unwrap();
    assert_eq!(p.rows.len(), 1);
    assert_eq!(p.next, None);
}

#[tokio::test]
async fn unsupported_clauses_are_diagnostics_and_a_wholly_unsupported_query_sends_nothing() {
    let (mut server, oa) = adapter().await;
    let m = server.mock("GET", "/works").expect(0).create_async().await;
    let p = oa
        .search(&Query::clause("container", "any", "Science"), page(5))
        .await
        .unwrap();
    m.assert_async().await;
    assert!(p.rows.is_empty());
    assert_eq!(p.diagnostics[0].status, Status::Unsupported);
    assert_eq!(p.diagnostics[0].detail, "bib.container any");
}

#[tokio::test]
async fn related_walks_cited_by_cites_and_related_to_and_pages_by_cursor() {
    let (mut server, oa) = adapter().await;
    let id = format!("openalex:{W}");
    let cites = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("filter".into(), format!("cites:{W}")),
            Matcher::UrlEncoded("per-page".into(), "2".into()),
            Matcher::UrlEncoded("cursor".into(), "*".into()),
        ]))
        .with_status(200)
        .with_body(fixture("cites.json"))
        .create_async()
        .await;
    let p = oa.related(&id, Relation::CitedBy, page(2)).await.unwrap();
    cites.assert_async().await;
    assert_eq!(p.rows.len(), 2);
    assert_eq!(p.rows[0].ids["openalex"], "W2163197622");
    assert!(
        p.next.as_deref().unwrap().starts_with("Ils2MzYx"),
        "full page: a cursor"
    );

    let refs = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("filter".into(), format!("cited_by:{W}")),
            Matcher::UrlEncoded("cursor".into(), "CUR".into()),
        ]))
        .with_status(200)
        .with_body(fixture("cited-by.json"))
        .create_async()
        .await;
    let p = oa
        .related(
            &id,
            Relation::Cites,
            PageRequest {
                limit: 2,
                cursor: Some("CUR".into()),
                sort: None,
            },
        )
        .await
        .unwrap();
    refs.assert_async().await;
    assert_eq!(p.rows[0].ids["openalex"], "W3021622851");

    let sim = server
        .mock("GET", "/works")
        .match_query(Matcher::UrlEncoded(
            "filter".into(),
            format!("related_to:{W}"),
        ))
        .with_status(200)
        .with_body(fixture("cites.json"))
        .create_async()
        .await;
    oa.related(&id, Relation::Similar, page(2)).await.unwrap();
    sim.assert_async().await;

    let d = oa
        .related(&id, Relation::Coreads, PageRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        (d.status, d.detail.as_str()),
        (Status::Unsupported, "coreads")
    );
}

#[tokio::test]
async fn related_by_doi_resolves_the_work_id_first() {
    let (mut server, oa) = adapter().await;
    let fetch = server
        .mock("GET", "/works/https://doi.org/10.1126/science.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .expect(1)
        .create_async()
        .await;
    let list = server
        .mock("GET", "/works")
        .match_query(Matcher::UrlEncoded(
            "filter".into(),
            format!("cited_by:{W}"),
        ))
        .with_status(200)
        .with_body(fixture("cited-by.json"))
        .expect(1)
        .create_async()
        .await;
    let p = oa
        .related(&format!("doi:{DOI}"), Relation::Cites, page(2))
        .await
        .unwrap();
    fetch.assert_async().await;
    list.assert_async().await;
    assert_eq!(p.rows.len(), 2);
}

#[tokio::test]
async fn a_429_backs_off_and_is_never_retried() {
    let (mut server, oa) = adapter().await;
    let m = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_body(fixture("budget-exhausted-429.json"));
    let m = with_headers(m, "budget-exhausted-429.headers")
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "stars");
    let first = oa.search(&q, page(2)).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 7s"), "{}", first.detail);
    let second = oa.search(&q, page(2)).await.unwrap_err();
    assert!(second.detail.contains("nothing was sent"));
    m.assert_async().await;
    assert!(oa.limiter().backoff_remaining(SOURCE) > Duration::from_secs(5));
}

#[tokio::test]
async fn a_spent_daily_budget_backs_the_source_off_until_the_reset() {
    let (mut server, oa) = adapter().await;
    let _m = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("x-ratelimit-remaining", "0")
        .with_header("x-ratelimit-reset", "3600")
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "stars");
    assert!(oa.search(&q, page(3)).await.is_ok(), "this answer is fine");
    assert!(oa.limiter().backoff_remaining(SOURCE) > Duration::from_secs(3000));
    let d = oa.search(&q, page(3)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
}

#[tokio::test]
async fn a_server_error_is_an_error_diagnostic_and_export_is_unsupported() {
    let (mut server, oa) = adapter().await;
    let _m = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(503)
        .create_async()
        .await;
    let d = oa
        .search(&Query::clause("any", "any", "x"), page(1))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    let d = oa.export(&[], ExportFormat::Bibtex).await.unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end() {
    let (mut server, oa) = adapter().await;
    let _s = server
        .mock("GET", "/works")
        .match_query(Matcher::UrlEncoded("search".into(), "first star".into()))
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let _f = server
        .mock("GET", "/works/https://doi.org/10.1126/science.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(oa));
    let service = DefaultSourceService::with_registry(registry);
    let r = service
        .search(
            Query::clause("any", "any", "first star"),
            None,
            Some(3),
            None,
            None,
        )
        .await;
    let answer = r.answer.expect("ok");
    assert_eq!(answer.rows.len(), 3);
    assert!(answer.next.unwrap().contains("openalex"));
    let r = service.fetch(format!("https://doi.org/{DOI}"), None).await;
    assert_eq!(r.answer.unwrap().work.ids["openalex"], W);
}

/// Live, opt-in: `cargo test -p source-openalex -- --ignored live`. Needs
/// the network and, for the polite pool, `KOINE_SOURCE_CONTACT`.
#[tokio::test]
#[ignore = "reaches api.openalex.org"]
async fn live_search_and_fetch() {
    let oa = OpenAlex::new().with_limiter(Arc::new(Limiter::new()));
    let p = oa
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Abel"),
                    Query::clause("title", "any", "first star"),
                ],
            },
            page(3),
        )
        .await
        .unwrap();
    assert!(!p.rows.is_empty());
    assert!(p.rows.iter().all(|r| r.ids.contains_key("openalex")));
    let w = oa
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("Abel 2002 exists");
    assert_eq!(w.container_title.as_deref(), Some("Science"));
}
