//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches arxiv.org except
//! the one `#[ignore]` live test at the end.

use std::sync::Arc;
use std::time::Duration;

use koine_sources::kind::Version;
use koine_sources::{Query, Status};
use mockito::{Matcher, Server, ServerGuard};
use source_service::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use source_service::etiquette::{Admission, Limiter};
use source_service::service::{DefaultSourceService, SourceService};
use source_service::Registry;

use crate::{Arxiv, SOURCE};

const ID: &str = "astro-ph/0112088";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A mock-backed adapter whose limiter is fast, so a test is not slowed by
/// arXiv's real one-per-three-seconds (the etiquette test uses the real one).
async fn adapter() -> (ServerGuard, Arxiv) {
    let server = Server::new_async().await;
    let limiter = Arc::new(Limiter::new());
    let arxiv = Arxiv::new()
        .with_base_url(server.url())
        .with_limiter(limiter.clone())
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    limiter.set_limit(SOURCE, 100, Duration::from_secs(1));
    (server, arxiv)
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort: None,
    }
}

#[test]
fn the_explain_record_is_valid_and_declares_the_terms_of_use() {
    let record = crate::explain_json();
    assert_eq!(record.source, "arxiv");
    assert_eq!(record.kind, "bibliographic-work@1");
    assert_eq!(record.page.max, Some(100));
    assert!(record.secrets.is_empty());
    assert_eq!(record.etiquette.limit.as_deref(), Some("1 per 3s"));
    assert_eq!(record.indexes["subject"].exact, Some(true));
    for index in [
        "any", "title", "abstract", "author", "subject", "arxiv", "year",
    ] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    assert!(!record.indexes.contains_key("first_author"));
}

#[test]
fn detect_claims_old_and_new_arxiv_ids_only() {
    let a = Arxiv::new();
    for yes in [
        "arxiv:2101.00001",
        "arxiv:2101.00001v3",
        "arxiv:1706.03762",
        "arxiv:astro-ph/0112088",
        "arxiv:math.GT/0309136v2",
        "arxiv:hep-th/9901001",
    ] {
        assert!(a.detect(yes), "{yes}");
    }
    for no in [
        "doi:10.1126/science.1063991",
        "arxiv:2101",
        "arxiv:nope",
        "bibcode:2002Sci...295...93A",
        "2101.00001",
    ] {
        assert!(!a.detect(no), "{no}");
    }
}

#[tokio::test]
async fn the_limiter_is_seeded_with_one_request_per_three_seconds_and_a_second_request_waits() {
    let limiter = Arc::new(Limiter::new());
    let arxiv = Arxiv::new().with_limiter(limiter);
    assert_eq!(
        arxiv.limiter().limit_of(SOURCE),
        Some((1, Duration::from_secs(3)))
    );
    assert_eq!(arxiv.limiter().check(SOURCE), Admission::Now);
    match arxiv.limiter().check(SOURCE) {
        Admission::After(wait) => {
            assert!(wait > Duration::from_millis(2500), "{wait:?}")
        }
        other => panic!("the second request must wait: {other:?}"),
    }
}

#[tokio::test]
async fn a_search_sends_the_tree_start_max_results_and_the_contact_in_the_user_agent() {
    let (mut server, arxiv) = adapter().await;
    let m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded(
                "search_query".into(),
                "(au:Abel AND ti:\"first star\")".into(),
            ),
            Matcher::UrlEncoded("start".into(), "0".into()),
            Matcher::UrlEncoded("max_results".into(), "3".into()),
            Matcher::UrlEncoded("sortBy".into(), "relevance".into()),
            Matcher::UrlEncoded("sortOrder".into(), "descending".into()),
        ]))
        .match_header(
            "user-agent",
            Matcher::Regex("koine-source-arxiv/.*mailto:tabel@stanford.edu".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/atom+xml")
        .with_body(fixture("search.xml"))
        .expect(1)
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel"),
            Query::clause("title", "adj", "first star"),
        ],
    };
    let p = arxiv.search(&q, page(3)).await.unwrap();
    m.assert_async().await;
    assert_eq!(p.rows.len(), 3);
    assert!(p.diagnostics.is_empty());
    assert_eq!(p.next.as_deref(), Some("3"), "3 of totalResults 5");
    let row = &p.rows[0];
    assert_eq!(row.ty, "article");
    assert_eq!(
        row.title.as_deref(),
        Some("The Formation of the First Star in the Universe")
    );
    assert_eq!(row.ids["arxiv"], ID, "versionless");
    assert_eq!(row.ids["doi"], "10.1126/science.1063991");
    assert_eq!(row.doi.as_deref(), Some("10.1126/science.1063991"));
    assert_eq!(row.author.len(), 3);
    assert_eq!(row.author[0].family.as_deref(), Some("Abel"));
    assert_eq!(row.author[0].given.as_deref(), Some("Tom"));
    assert_eq!(row.author[2].family.as_deref(), Some("Norman"));
    assert_eq!(row.author[2].given.as_deref(), Some("Michael L."));
    assert_eq!(
        row.issued.as_ref().unwrap().date_parts,
        vec![vec![2001, 12, 4]]
    );
    let abs = row.abstract_.as_deref().unwrap();
    assert!(abs.starts_with("We describe results"));
    assert!(abs.contains("n>10^9 cm^-3"), "entities decoded: {abs}");
    assert!(abs.contains("(M >> 1 solar mass)"));
    assert_eq!(row.locations.len(), 1);
    let loc = &row.locations[0];
    assert_eq!(loc.version, Version::Submitted);
    assert_eq!(loc.source_id, "astro-ph/0112088v1");
    assert_eq!(
        loc.pdf.as_deref(),
        Some("https://arxiv.org/pdf/astro-ph/0112088v1")
    );
    assert!(loc.primary);
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(prov.source, SOURCE);
    assert!(prov.query.as_deref().unwrap().contains("first star"));
    assert_eq!(p.rows[1].doi.as_deref(), Some("10.1063/1.2940787"));
    assert_eq!(p.rows[1].ids["arxiv"], "0801.2124");
    for r in &p.rows {
        r.validate().unwrap();
    }
}

#[tokio::test]
async fn the_cursor_is_the_next_offset_and_the_last_page_has_none() {
    let (mut server, arxiv) = adapter().await;
    let m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("start".into(), "3".into()),
            Matcher::UrlEncoded("max_results".into(), "3".into()),
            Matcher::UrlEncoded("sortBy".into(), "submittedDate".into()),
        ]))
        .with_status(200)
        .with_body(fixture("search.xml")) // 3 entries, totalResults 5
        .create_async()
        .await;
    let p = arxiv
        .search(
            &Query::clause("title", "any", "first star"),
            PageRequest {
                limit: 3,
                cursor: Some("3".into()),
                sort: Some("year".into()),
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    // start 3 + 3 rows = 6 >= totalResults 5: the end.
    assert_eq!(p.next, None);
    // And the cap: more than 100 is asked as 100.
    let m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::UrlEncoded("max_results".into(), "100".into()))
        .with_status(200)
        .with_body(fixture("search.xml"))
        .create_async()
        .await;
    arxiv
        .search(&Query::clause("any", "any", "x"), page(5000))
        .await
        .unwrap();
    m.assert_async().await;
}

#[tokio::test]
async fn an_unsupported_clause_is_a_diagnostic_and_a_wholly_unsupported_query_sends_nothing() {
    let (mut server, arxiv) = adapter().await;
    let m = server
        .mock("GET", "/api/query")
        .expect(0)
        .create_async()
        .await;
    let p = arxiv
        .search(&Query::clause("first_author", "any", "Abel"), page(5))
        .await
        .unwrap();
    m.assert_async().await;
    assert!(p.rows.is_empty());
    assert_eq!(p.diagnostics[0].status, Status::Unsupported);
    assert_eq!(p.diagnostics[0].detail, "bib.first_author any");
}

#[tokio::test]
async fn an_arxiv_clause_goes_to_id_list_beside_the_search() {
    let (mut server, arxiv) = adapter().await;
    let m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("search_query".into(), "ti:star".into()),
            Matcher::UrlEncoded("id_list".into(), ID.into()),
        ]))
        .with_status(200)
        .with_body(fixture("fetch.xml"))
        .expect(1)
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("title", "any", "star"),
            Query::clause("arxiv", "=", ID),
        ],
    };
    let p = arxiv.search(&q, page(5)).await.unwrap();
    m.assert_async().await;
    assert_eq!(p.rows.len(), 1);
}

#[tokio::test]
async fn a_fetch_by_arxiv_id_uses_id_list_and_a_miss_is_none() {
    let (mut server, arxiv) = adapter().await;
    let m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("id_list".into(), ID.into()),
            Matcher::UrlEncoded("max_results".into(), "1".into()),
        ]))
        .with_status(200)
        .with_body(fixture("fetch.xml"))
        .create_async()
        .await;
    let work = arxiv
        .fetch(&format!("arxiv:{ID}"))
        .await
        .unwrap()
        .expect("found");
    m.assert_async().await;
    assert_eq!(work.ids["arxiv"], ID);
    assert_eq!(work.ids["doi"], "10.1126/science.1063991");
    assert_eq!(
        work.url.as_deref(),
        Some("https://arxiv.org/abs/astro-ph/0112088v1")
    );
    work.validate().unwrap();

    // A modern id with no DOI and a later version.
    let _n = server
        .mock("GET", "/api/query")
        .match_query(Matcher::UrlEncoded("id_list".into(), "1706.03762v7".into()))
        .with_status(200)
        .with_body(fixture("fetch-new.xml"))
        .create_async()
        .await;
    let w = arxiv.fetch("arxiv:1706.03762v7").await.unwrap().unwrap();
    assert_eq!(w.ids["arxiv"], "1706.03762");
    assert_eq!(w.locations[0].source_id, "1706.03762v7");
    assert_eq!(w.title.as_deref(), Some("Attention Is All You Need"));
    assert_eq!(w.author.len(), 8);
    assert!(
        w.doi.is_none() && !w.ids.contains_key("doi"),
        "no arxiv:doi"
    );

    let _e = server
        .mock("GET", "/api/query")
        .match_query(Matcher::UrlEncoded("id_list".into(), "9999.99999".into()))
        .with_status(200)
        .with_body(fixture("empty.xml"))
        .create_async()
        .await;
    assert_eq!(arxiv.fetch("arxiv:9999.99999").await.unwrap(), None);
    assert_eq!(arxiv.fetch("doi:10.1/x").await.unwrap(), None, "not ours");
}

#[tokio::test]
async fn arxivs_error_entry_is_an_error_diagnostic() {
    let (mut server, arxiv) = adapter().await;
    let _m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("error.xml"))
        .create_async()
        .await;
    let d = arxiv
        .search(&Query::clause("any", "any", "x"), page(1))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("incorrect id format"), "{}", d.detail);
}

#[tokio::test]
async fn a_429_backs_off_and_is_never_retried() {
    let (mut server, arxiv) = adapter().await;
    let m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_header("retry-after", "3")
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "stars");
    let first = arxiv.search(&q, page(1)).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 3s"), "{}", first.detail);
    let second = arxiv.search(&q, page(1)).await.unwrap_err();
    assert_eq!(second.status, Status::RateLimited);
    assert!(second.detail.contains("nothing was sent"));
    m.assert_async().await;
}

#[tokio::test]
async fn a_server_error_is_an_error_diagnostic() {
    let (mut server, arxiv) = adapter().await;
    let _m = server
        .mock("GET", "/api/query")
        .match_query(Matcher::Any)
        .with_status(503)
        .create_async()
        .await;
    let d = arxiv
        .search(&Query::clause("any", "any", "x"), page(1))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("503"));
}

#[tokio::test]
async fn related_and_export_are_unsupported_not_empty() {
    let arxiv = Arxiv::new();
    let d = arxiv
        .related(
            &format!("arxiv:{ID}"),
            Relation::Cites,
            PageRequest::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
    assert_eq!(d.detail, "cites");
    let d = arxiv.export(&[], ExportFormat::Bibtex).await.unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end() {
    let (mut server, arxiv) = adapter().await;
    let _s = server
        .mock("GET", "/api/query")
        .match_query(Matcher::UrlEncoded(
            "search_query".into(),
            "(ti:first OR ti:star)".into(),
        ))
        .with_status(200)
        .with_body(fixture("search.xml"))
        .create_async()
        .await;
    let _f = server
        .mock("GET", "/api/query")
        .match_query(Matcher::UrlEncoded("id_list".into(), ID.into()))
        .with_status(200)
        .with_body(fixture("fetch.xml"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(arxiv));
    let service = DefaultSourceService::with_registry(registry);
    let r = service
        .search(
            Query::clause("title", "any", "first star"),
            None,
            Some(3),
            None,
            None,
            None,
        )
        .await;
    let answer = r.answer.expect("ok");
    assert_eq!(answer.rows.len(), 3);
    assert!(answer.next.unwrap().contains("arxiv"));
    let r = service
        .fetch(format!("https://arxiv.org/abs/{ID}"), None, None)
        .await;
    assert_eq!(
        r.answer.unwrap().work.title.as_deref(),
        Some("The Formation of the First Star in the Universe")
    );
}

/// Live, opt-in: `cargo test -p source-arxiv -- --ignored live`. Needs the
/// network; the real limiter (one request per three seconds) applies.
#[tokio::test]
#[ignore = "reaches export.arxiv.org"]
async fn live_search_and_fetch() {
    let arxiv = Arxiv::new().with_limiter(Arc::new(Limiter::new()));
    let p = arxiv
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Abel"),
                    Query::clause("title", "adj", "first star"),
                ],
            },
            page(3),
        )
        .await
        .unwrap();
    assert!(!p.rows.is_empty());
    assert!(p.rows.iter().all(|r| r.ids.contains_key("arxiv")));
    let work = arxiv
        .fetch(&format!("arxiv:{ID}"))
        .await
        .unwrap()
        .expect("Abel 2001 exists");
    assert_eq!(work.doi.as_deref(), Some("10.1126/science.1063991"));
}
