//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches api.crossref.org
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

use crate::{Crossref, SOURCE};

const DOI: &str = "10.1126/science.1063991";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The headers of a recorded response, as `(name, value)` pairs.
fn headers(name: &str) -> Vec<(String, String)> {
    fixture(name)
        .lines()
        .filter_map(|l| {
            l.split_once(": ")
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .filter(|(k, _)| k.starts_with("x-rate-limit") || k == "retry-after" || k == "content-type")
        .collect()
}

async fn adapter() -> (ServerGuard, Crossref) {
    let server = Server::new_async().await;
    let crossref = Crossref::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    (server, crossref)
}

fn mock_with_headers(m: mockito::Mock, header_file: &str) -> mockito::Mock {
    let mut m = m;
    for (k, v) in headers(header_file) {
        m = m.with_header(&k, &v);
    }
    m
}

#[test]
fn the_explain_record_is_the_plans_and_valid() {
    let record = crate::explain_json();
    assert_eq!(record.source, "crossref");
    assert_eq!(record.version, crate::API_VERSION);
    assert_eq!(record.kind, "bibliographic-work@1");
    assert!(record.indexes.contains_key("year"));
    assert_eq!(record.indexes["doi"].exact, Some(true));
    assert_eq!(record.secrets, ["crossref-plus-token"]);
    assert_eq!(record.etiquette.contact.as_deref(), Some("mailto"));
    assert_eq!(record.page.max, Some(1000));
    assert!(Crossref::new().detect("doi:10.1126/science.1063991"));
    assert!(!Crossref::new().detect("arxiv:2101.00001"));
    assert!(!Crossref::new().detect("doi:nope"));
}

#[tokio::test]
async fn a_search_sends_bibliographic_author_cursor_rows_and_mailto_and_reads_the_rate_headers() {
    let (mut server, crossref) = adapter().await;
    let m = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query.bibliographic".into(), "reionization 21cm".into()),
            Matcher::UrlEncoded("query.author".into(), "Abel".into()),
            Matcher::UrlEncoded("rows".into(), "3".into()),
            Matcher::UrlEncoded("cursor".into(), "*".into()),
            Matcher::UrlEncoded("sort".into(), "relevance".into()),
            Matcher::UrlEncoded("order".into(), "desc".into()),
            Matcher::UrlEncoded("mailto".into(), "tabel@stanford.edu".into()),
        ]))
        .match_header(
            "user-agent",
            Matcher::Regex("koine-source-crossref/.*mailto:tabel@stanford.edu".into()),
        )
        .with_status(200)
        .with_body(fixture("search.json"));
    let m = mock_with_headers(m, "search.headers")
        .expect(1)
        .create_async()
        .await;

    let q = Query::And {
        and: vec![
            Query::clause("title", "any", "reionization 21cm"),
            Query::clause("author", "any", "Abel"),
        ],
    };
    let page = crossref
        .search(
            &q,
            PageRequest {
                limit: 3,
                cursor: None,
                sort: None,
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert_eq!(page.rows.len(), 3);
    assert!(page.diagnostics.is_empty());
    assert_eq!(
        page.next.as_deref(),
        Some("MTAyNzg5NTY3NDAwMCwxMC4xMDg2JTJGMzQwNDUx")
    );
    let row = &page.rows[0];
    assert_eq!(row.doi.as_deref(), Some("10.1016/s1384-1076(01)00065-3"));
    assert_eq!(row.ids["doi"], "10.1016/s1384-1076(01)00065-3");
    assert_eq!(
        row.title.as_deref(),
        Some("Simulating reionization in numerical cosmology")
    );
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(prov.source, SOURCE);
    assert_eq!(prov.source_id, "10.1016/s1384-1076(01)00065-3");
    assert!(prov.query.as_deref().unwrap().contains("reionization"));
    row.validate().unwrap();
    // The headers (x-rate-limit-limit: 3, x-rate-limit-interval: 1s) refilled the bucket.
    assert_eq!(
        crossref.limiter().limit_of(SOURCE),
        Some((3, Duration::from_secs(1)))
    );
}

#[tokio::test]
async fn year_within_is_a_pub_date_filter_and_a_short_page_has_no_next() {
    let (mut server, crossref) = adapter().await;
    let m = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query.author".into(), "Abel".into()),
            Matcher::UrlEncoded(
                "filter".into(),
                "from-pub-date:2002-01-01,until-pub-date:2002-12-31".into(),
            ),
            Matcher::UrlEncoded("rows".into(), "10".into()),
        ]))
        .with_status(200)
        .with_body(fixture("filter.json"));
    let m = mock_with_headers(m, "filter.headers").create_async().await;
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel"),
            Query::clause("year", "within", "2002 2002"),
        ],
    };
    let page = crossref
        .search(
            &q,
            PageRequest {
                limit: 10,
                cursor: None,
                sort: None,
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.next, None, "two rows of ten asked: the end");
    assert!(page
        .rows
        .iter()
        .all(|r| r.issued.as_ref().unwrap().date_parts[0][0] == 2002));
}

#[tokio::test]
async fn an_unsupported_clause_is_a_diagnostic_and_a_wholly_unsupported_query_sends_nothing() {
    let (mut server, crossref) = adapter().await;
    let m = server.mock("GET", "/works").expect(0).create_async().await;
    let q = Query::clause("first_author", "any", "Abel");
    let page = crossref
        .search(
            &q,
            PageRequest {
                limit: 5,
                cursor: None,
                sort: None,
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert!(page.rows.is_empty());
    assert_eq!(page.diagnostics[0].status, Status::Unsupported);
    assert_eq!(page.diagnostics[0].detail, "bib.first_author any");
}

#[tokio::test]
async fn a_fetch_by_doi_parses_the_work() {
    let (mut server, crossref) = adapter().await;
    let m = server
        .mock("GET", "/works/10.1126%2Fscience.1063991")
        .match_query(Matcher::UrlEncoded(
            "mailto".into(),
            "tabel@stanford.edu".into(),
        ))
        .with_status(200)
        .with_body(fixture("fetch.json"));
    let m = mock_with_headers(m, "fetch.headers").create_async().await;
    let work = crossref
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("found");
    m.assert_async().await;
    assert_eq!(work.ty, "article-journal");
    assert_eq!(
        work.title.as_deref(),
        Some("The Formation of the First Star in the Universe")
    );
    assert_eq!(work.author.len(), 3);
    assert_eq!(work.author[0].family.as_deref(), Some("Abel"));
    assert_eq!(work.author[0].given.as_deref(), Some("Tom"));
    assert_eq!(
        work.issued.as_ref().unwrap().date_parts,
        vec![vec![2002, 1, 4]]
    );
    assert_eq!(work.container_title.as_deref(), Some("Science"));
    assert_eq!(work.volume.as_deref(), Some("295"));
    assert_eq!(work.issue.as_deref(), Some("5552"));
    assert_eq!(work.page.as_deref(), Some("93-98"));
    assert_eq!(work.language.as_deref(), Some("en"));
    assert!(
        work.abstract_
            .as_deref()
            .unwrap()
            .starts_with("We describe results"),
        "JATS stripped"
    );
    assert_eq!(work.doi.as_deref(), Some(DOI));
    assert_eq!(
        work.url.as_deref(),
        Some("https://doi.org/10.1126/science.1063991")
    );
    assert_eq!(work.ids["doi"], DOI);
    assert_eq!(work.locations.len(), 1);
    let loc = &work.locations[0];
    assert_eq!(loc.source, SOURCE);
    assert_eq!(loc.source_id, DOI);
    assert_eq!(loc.version, Version::Published, "content-version vor");
    assert!(loc.primary);
    assert_eq!(
        loc.pdf.as_deref(),
        Some("https://www.science.org/doi/pdf/10.1126/science.1063991")
    );
    let cited = work.metrics.as_ref().unwrap().cited_by.as_ref().unwrap();
    assert_eq!(cited.value, 1188);
    assert_eq!(cited.source, SOURCE);
    assert_eq!(work.provenance.as_ref().unwrap().source_id, DOI);
    work.validate().unwrap();
    let csl = work.to_csl_json();
    assert_eq!(csl["id"], json!(DOI));
    assert_eq!(csl["custom"]["ids"]["doi"], json!(DOI));
    assert_eq!(csl["custom"]["metrics"]["cited_by"]["value"], json!(1188));
}

#[test]
fn content_versions_map_to_driver_versions() {
    assert_eq!(crate::parse::version_of(Some("vor")), Version::Published);
    assert_eq!(crate::parse::version_of(Some("am")), Version::Accepted);
    assert_eq!(crate::parse::version_of(Some("tdm")), Version::Submitted);
    assert_eq!(crate::parse::version_of(None), Version::Submitted);
    let item = json!({
        "DOI": "10.1/x", "type": "posted-content", "title": ["P"],
        "link": [
            {"URL": "https://a/x.pdf", "content-type": "application/pdf", "content-version": "am"},
            {"URL": "https://a/x", "content-type": "text/html", "content-version": "vor"}
        ],
        "relation": {"is-preprint-of": [{"id-type": "doi", "id": "10.1/y"}]},
        "created": {"date-parts": [[2021, 2]]}
    });
    let work = crate::parse::work(&item, "2026-10-19T00:00:00Z", None).unwrap();
    assert_eq!(work.ty, "article");
    assert_eq!(work.locations.len(), 2);
    assert_eq!(work.locations[1].version, Version::Accepted);
    assert_eq!(work.locations[1].pdf.as_deref(), Some("https://a/x.pdf"));
    assert!(!work.locations[1].primary);
    assert_eq!(
        work.issued.as_ref().unwrap().date_parts,
        vec![vec![2021, 2]]
    );
    work.validate().unwrap();
}

#[tokio::test]
async fn a_fetch_miss_is_none() {
    let (mut server, crossref) = adapter().await;
    let _m = server
        .mock("GET", "/works/10.1126%2Fnope")
        .match_query(Matcher::Any)
        .with_status(404)
        .with_body(r#"{"status":"error","message-type":"work-not-found"}"#)
        .create_async()
        .await;
    assert_eq!(crossref.fetch("doi:10.1126/nope").await.unwrap(), None);
    assert_eq!(crossref.fetch("arxiv:1").await.unwrap(), None, "not ours");
}

#[tokio::test]
async fn a_429_backs_off_and_is_never_retried() {
    let (mut server, crossref) = adapter().await;
    let m = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_body(fixture("rate-limited-429.json"));
    let m = mock_with_headers(m, "rate-limited-429.headers")
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "reionization");
    let req = || PageRequest {
        limit: 2,
        cursor: None,
        sort: None,
    };
    let first = crossref.search(&q, req()).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 2s"), "{}", first.detail);
    // While backed off, the second call sends nothing and says so.
    let second = crossref.search(&q, req()).await.unwrap_err();
    assert_eq!(second.status, Status::RateLimited);
    assert!(
        second.detail.contains("nothing was sent"),
        "{}",
        second.detail
    );
    m.assert_async().await;
    assert!(crossref.limiter().backoff_remaining(SOURCE) > Duration::from_secs(1));
}

#[tokio::test]
async fn a_server_error_is_an_error_diagnostic_not_a_panic() {
    let (mut server, crossref) = adapter().await;
    let _m = server
        .mock("GET", "/works")
        .match_query(Matcher::Any)
        .with_status(503)
        .with_body("down")
        .create_async()
        .await;
    let d = crossref
        .search(
            &Query::clause("any", "any", "x"),
            PageRequest {
                limit: 1,
                cursor: None,
                sort: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("503"));
}

#[tokio::test]
async fn export_negotiates_bibtex_and_emits_csl_json_natively() {
    let (mut server, crossref) = adapter().await;
    let bib = server
        .mock("GET", "/works/10.1126%2Fscience.1063991/transform")
        .match_query(Matcher::Any)
        .match_header("accept", "application/x-bibtex")
        .with_status(200)
        .with_body(fixture("export.bib"))
        .create_async()
        .await;
    let text = crossref
        .export(&[format!("doi:{DOI}")], ExportFormat::Bibtex)
        .await
        .unwrap();
    bib.assert_async().await;
    assert!(text.starts_with("@article{Abel_2002"));
    assert!(text.contains("DOI={10.1126/science.1063991}"));

    let _work = server
        .mock("GET", "/works/10.1126%2Fscience.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let text = crossref
        .export(&[format!("doi:{DOI}")], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["type"], json!("article-journal"));
    assert_eq!(v[0]["container-title"], json!("Science"));
    assert_eq!(v[0]["custom"]["ids"]["doi"], json!(DOI));
}

#[tokio::test]
async fn related_cites_are_the_references_with_dois_and_cited_by_is_unsupported() {
    let (mut server, crossref) = adapter().await;
    let _m = server
        .mock("GET", "/works/10.1126%2Fscience.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let id = format!("doi:{DOI}");
    let page = crossref
        .related(
            &id,
            Relation::Cites,
            PageRequest {
                limit: 5,
                cursor: None,
                sort: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 5);
    assert!(page.rows.iter().all(|r| r.ids.contains_key("doi")));
    let next = page.next.expect("more references");
    let page2 = crossref
        .related(
            &id,
            Relation::Cites,
            PageRequest {
                limit: 100,
                cursor: Some(next),
                sort: None,
            },
        )
        .await
        .unwrap();
    assert!(!page2.rows.is_empty());
    assert_eq!(page2.next, None);
    let d = crossref
        .related(&id, Relation::CitedBy, PageRequest::default())
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
    assert_eq!(d.detail, "cited_by");
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end() {
    let (mut server, crossref) = adapter().await;
    let _m = server
        .mock("GET", "/works")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query.bibliographic".into(), "reionization 21cm".into()),
            Matcher::UrlEncoded("rows".into(), "3".into()),
        ]))
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let _f = server
        .mock("GET", "/works/10.1126%2Fscience.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(crossref));
    let service = DefaultSourceService::with_registry(registry);

    let explain = service.explain(None).await;
    assert_eq!(explain.answer.unwrap().sources[0].source, "crossref");

    let r = service
        .search(
            Query::clause("title", "any", "reionization 21cm"),
            None,
            Some(3),
            None,
            None,
        )
        .await;
    let answer = r.answer.expect("ok");
    assert_eq!(answer.rows.len(), 3);
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.source == "crossref" && d.status == Status::Done));
    assert!(answer.next.unwrap().contains("crossref"));

    let r = service.fetch(format!("https://doi.org/{DOI}"), None).await;
    assert_eq!(
        r.answer.unwrap().work.title.as_deref(),
        Some("The Formation of the First Star in the Universe")
    );

    let r = service
        .match_citation(
            source_service::dto::MatchInput {
                text: Some(format!(
                    "Abel, Bryan & Norman 2002, Science 295, 93, doi:{DOI}"
                )),
                ..Default::default()
            },
            None,
        )
        .await;
    let answer = r.answer.unwrap();
    assert!(answer.accepted);
    assert_eq!(answer.candidates[0].score, Some(1.0));
    assert_eq!(answer.candidates[0].work.doi.as_deref(), Some(DOI));
}

/// Live, opt-in: `cargo test -p source-crossref -- --ignored live`. Needs
/// the network and, for the polite pool, `KOINE_SOURCE_CONTACT`.
#[tokio::test]
#[ignore = "reaches api.crossref.org"]
async fn live_search_and_fetch() {
    let crossref = Crossref::new().with_limiter(Arc::new(Limiter::new()));
    let page = crossref
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Abel"),
                    Query::clause("title", "any", "first star"),
                ],
            },
            PageRequest {
                limit: 3,
                cursor: None,
                sort: None,
            },
        )
        .await
        .unwrap();
    assert!(!page.rows.is_empty());
    assert!(page.rows.iter().all(|r| r.ids.contains_key("doi")));
    let work = crossref
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("Abel 2002 exists");
    assert_eq!(work.container_title.as_deref(), Some("Science"));
}
