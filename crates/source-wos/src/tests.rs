//! Tier A over the fixtures (`fixtures/README.md`: **hand-written** from the
//! documented Starter API shape, no key was reachable). The HTTP layer is a
//! mock server; nothing here reaches api.clarivate.com except the one
//! `#[ignore]` live test at the end.

use std::sync::Arc;
use std::time::Duration;

use impress_service_core::secrets::{self, Secret, SecretSource};
use koine_sources::kind::Version;
use koine_sources::{Query, Status};
use mockito::{Matcher, Server, ServerGuard};
use source_service::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use source_service::etiquette::Limiter;
use source_service::service::{BibSourceService, DefaultBibSourceService};
use source_service::Registry;

use crate::{Wos, SOURCE};

const DOI: &str = "10.1126/science.1063991";
const UT: &str = "WOS:000000000000001";
const KEY: &str = "test-wos-key-123";

/// The secret slot is process-wide: tests that install one run one at a time.
static SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Holds(Option<&'static str>);
impl SecretSource for Holds {
    fn get(&self, name: &str) -> Option<Secret> {
        (name == "wos-api-key")
            .then(|| self.0.map(Secret::new))
            .flatten()
    }
    fn names(&self) -> Vec<String> {
        self.0
            .map(|_| vec!["wos-api-key".to_string()])
            .unwrap_or_default()
    }
}

/// Install the key (or none) for the length of one test.
async fn with_key(key: Option<&'static str>) -> tokio::sync::MutexGuard<'static, ()> {
    let guard = SLOT.lock().await;
    secrets::install(Arc::new(Holds(key)));
    guard
}

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

async fn adapter() -> (ServerGuard, Wos) {
    let server = Server::new_async().await;
    let wos = Wos::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    (server, wos)
}

fn serve(
    server: &mut ServerGuard,
    path: &str,
    query: Matcher,
    status: usize,
    name: &str,
) -> mockito::Mock {
    let mut m = server
        .mock("GET", path)
        .match_query(query)
        .with_status(status)
        .with_body(fixture(&format!("{name}.json")));
    if std::path::Path::new(&format!(
        "{}/fixtures/{name}.headers",
        env!("CARGO_MANIFEST_DIR")
    ))
    .exists()
    {
        for (k, v) in headers(&format!("{name}.headers")) {
            m = m.with_header(&k, &v);
        }
    }
    m
}

fn q(k: &str, v: &str) -> Matcher {
    Matcher::UrlEncoded(k.into(), v.into())
}

fn page(limit: u32, cursor: Option<&str>) -> PageRequest {
    PageRequest {
        limit,
        cursor: cursor.map(str::to_string),
        sort: None,
    }
}

#[test]
fn the_explain_record_is_valid_and_detect_claims_uts_and_dois() {
    let record = crate::explain_json();
    assert_eq!(record.source, "wos");
    assert_eq!(record.version, crate::API_VERSION);
    for index in [
        "any",
        "title",
        "author",
        "year",
        "doi",
        "container",
        "keyword",
    ] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    assert!(!record.indexes.contains_key("abstract"));
    assert_eq!(record.secrets, ["wos-api-key"]);
    assert_eq!(record.etiquette.key.as_deref(), Some("required"));
    assert_eq!(record.page.max, Some(50));
    assert_eq!(record.identifiers, ["wos", "doi"]);
    let w = Wos::new();
    assert!(w.detect("wos:WOS:000175157300021"));
    assert!(w.detect("WOS:000175157300021"));
    assert!(w.detect("wos:000175157300021"));
    assert!(w.detect("doi:10.1126/science.1063991"));
    assert!(!w.detect("wos:123"));
    assert!(!w.detect("pmid:11711636"));
}

#[tokio::test]
async fn without_a_key_search_and_fetch_are_refused_and_nothing_is_sent() {
    let _slot = with_key(None).await;
    let (mut server, wos) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let d = wos
        .search(&Query::clause("any", "any", "x"), page(5, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Refused);
    assert!(d.detail.contains("wos-api-key"), "{}", d.detail);
    let d = wos.fetch(&format!("doi:{DOI}")).await.unwrap_err();
    assert_eq!(d.status, Status::Refused);
    m.assert_async().await;
}

#[tokio::test]
async fn a_search_sends_the_key_header_field_tags_limit_page_and_sort() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let m = serve(
        &mut server,
        "/documents",
        Matcher::AllOf(vec![
            q("q", "(AU=(Abel T) AND TI=(first AND star))"),
            q("db", "WOS"),
            q("limit", "3"),
            q("page", "1"),
            q("sortField", "RS+D"),
        ]),
        200,
        "search",
    )
    .match_header("x-apikey", KEY)
    .match_header("accept", "application/json")
    .match_header(
        "user-agent",
        Matcher::Regex("koine-source-wos/.*mailto:tabel@stanford.edu".into()),
    )
    .expect(1)
    .create_async()
    .await;
    let query = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel, T."),
            Query::clause("title", "any", "first star"),
        ],
    };
    let got = wos.search(&query, page(3, None)).await.unwrap();
    m.assert_async().await;
    assert!(got.diagnostics.is_empty());
    assert_eq!(got.rows.len(), 3);
    assert_eq!(got.next.as_deref(), Some("2"), "3 of 13: page 2 next");
    let row = &got.rows[0];
    assert_eq!(row.ids["wos"], UT);
    assert_eq!(row.ids["doi"], DOI);
    assert_eq!(row.ids["pmid"], "11711636");
    assert_eq!(row.doi.as_deref(), Some(DOI));
    assert_eq!(row.ty, "article-journal");
    assert_eq!(
        row.title.as_deref(),
        Some("The formation of the first star in the universe")
    );
    assert_eq!(row.author.len(), 3);
    assert_eq!(row.author[0].family.as_deref(), Some("Abel"));
    assert_eq!(row.author[0].given.as_deref(), Some("T"));
    assert_eq!(row.issued.as_ref().unwrap().date_parts, vec![vec![2002, 1]]);
    assert_eq!(row.container_title.as_deref(), Some("SCIENCE"));
    assert_eq!(row.volume.as_deref(), Some("295"));
    assert_eq!(row.issue.as_deref(), Some("5552"));
    assert_eq!(row.page.as_deref(), Some("93-98"));
    let cited = row.metrics.as_ref().unwrap().cited_by.as_ref().unwrap();
    assert_eq!((cited.value, cited.source.as_str()), (1000, "wos"));
    assert_eq!(row.locations.len(), 1);
    assert_eq!(row.locations[0].version, Version::Published);
    assert!(row.locations[0].primary);
    assert_eq!(
        row.locations[0].url.as_deref(),
        Some("https://doi.org/10.1126/science.1063991")
    );
    assert!(row
        .url
        .as_deref()
        .unwrap()
        .contains("KeyUT=WOS:000000000000001"));
    assert_eq!(got.rows[2].ty, "paper-conference");
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(
        (prov.source.as_str(), prov.source_id.as_str()),
        (SOURCE, UT)
    );
    assert!(prov.query.as_deref().unwrap().contains("first star"));
    for r in &got.rows {
        r.validate().unwrap();
    }
    // X-RateLimit-Limit-Second: 5 refilled the bucket.
    assert_eq!(
        wos.limiter().limit_of(SOURCE),
        Some((5, Duration::from_secs(1)))
    );
}

#[tokio::test]
async fn paging_is_the_page_number_and_the_limit_is_capped_at_50() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    // Page 5 of limit 3 against total 13: rows 13..13, the end.
    let body = fixture("search.json")
        .replace("\"page\": 1", "\"page\": 5")
        .replace("\"limit\": 3", "\"limit\": 50");
    let m = server
        .mock("GET", "/documents")
        .match_query(Matcher::AllOf(vec![
            q("limit", "50"),
            q("page", "5"),
            q("sortField", "TC+D"),
        ]))
        .with_status(200)
        .with_body(body)
        .expect(1)
        .create_async()
        .await;
    let got = wos
        .search(
            &Query::clause("any", "any", "stars"),
            PageRequest {
                limit: 500,
                cursor: Some("5".into()),
                sort: Some("cited_by".into()),
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert_eq!(got.rows.len(), 3);
    // (5-1)*50 + 3 = 203 >= 13: no next.
    assert_eq!(got.next, None);
}

#[tokio::test]
async fn an_unsupported_clause_is_a_diagnostic_and_a_wholly_unsupported_query_sends_nothing() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let got = wos
        .search(&Query::clause("abstract", "any", "star"), page(5, None))
        .await
        .unwrap();
    m.assert_async().await;
    assert!(got.rows.is_empty());
    assert_eq!(got.diagnostics[0].status, Status::Unsupported);
    assert_eq!(got.diagnostics[0].detail, "bib.abstract any");
}

#[tokio::test]
async fn an_empty_search_is_no_rows() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let _m = serve(&mut server, "/documents", Matcher::Any, 200, "empty-search")
        .create_async()
        .await;
    let got = wos
        .search(&Query::clause("title", "any", "zzqxjk"), page(3, None))
        .await
        .unwrap();
    assert!(got.rows.is_empty());
    assert_eq!(got.next, None);
}

#[tokio::test]
async fn a_fetch_by_ut_is_get_documents_uid_and_a_doi_is_a_one_row_search() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let by_ut = serve(
        &mut server,
        "/documents/WOS%3A000000000000001",
        Matcher::Any,
        200,
        "fetch",
    )
    .match_header("x-apikey", KEY)
    .expect(1)
    .create_async()
    .await;
    let work = wos
        .fetch(&format!("wos:{UT}"))
        .await
        .unwrap()
        .expect("found");
    by_ut.assert_async().await;
    assert_eq!(work.ids["wos"], UT);
    work.validate().unwrap();

    let by_doi = serve(
        &mut server,
        "/documents",
        Matcher::AllOf(vec![
            q("q", "DO=(10.1126/science.1063991)"),
            q("limit", "1"),
        ]),
        200,
        "doi-lookup",
    )
    .expect(1)
    .create_async()
    .await;
    let work = wos.fetch(&format!("doi:{DOI}")).await.unwrap().unwrap();
    by_doi.assert_async().await;
    assert_eq!(work.ids["wos"], UT);
}

#[tokio::test]
async fn a_fetch_miss_is_none() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let _m = serve(
        &mut server,
        "/documents/WOS%3A000000000000009",
        Matcher::Any,
        404,
        "not-found-404",
    )
    .create_async()
    .await;
    assert_eq!(wos.fetch("wos:WOS:000000000000009").await.unwrap(), None);
    assert_eq!(wos.fetch("arxiv:1").await.unwrap(), None, "not ours");
}

#[tokio::test]
async fn a_rejected_key_is_refused_not_an_error() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let _m = serve(
        &mut server,
        "/documents",
        Matcher::Any,
        401,
        "unauthorized-401",
    )
    .create_async()
    .await;
    let d = wos
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Refused);
    assert!(d.detail.contains("401"));
    assert!(!d.detail.contains("test-wos-key"));
}

#[tokio::test]
async fn a_429_backs_off_and_is_never_retried() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let m = serve(
        &mut server,
        "/documents",
        Matcher::Any,
        429,
        "rate-limited-429",
    )
    .expect(1)
    .create_async()
    .await;
    let query = Query::clause("any", "any", "stars");
    let first = wos.search(&query, page(2, None)).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 2s"), "{}", first.detail);
    let second = wos.search(&query, page(2, None)).await.unwrap_err();
    assert_eq!(second.status, Status::RateLimited);
    assert!(
        second.detail.contains("nothing was sent"),
        "{}",
        second.detail
    );
    m.assert_async().await;
    assert!(wos.limiter().backoff_remaining(SOURCE) > Duration::from_secs(1));
}

#[tokio::test]
async fn a_server_error_is_an_error_diagnostic_not_a_panic() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let _m = server
        .mock("GET", "/documents")
        .match_query(Matcher::Any)
        .with_status(503)
        .with_body("down")
        .create_async()
        .await;
    let d = wos
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("503"));
}

#[tokio::test]
async fn related_is_unsupported_and_export_is_csl_json_only() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let d = wos
        .related(UT, Relation::CitedBy, PageRequest::default())
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
    assert_eq!(d.detail, "cited_by");
    let _m = serve(
        &mut server,
        "/documents/WOS%3A000000000000001",
        Matcher::Any,
        200,
        "fetch",
    )
    .create_async()
    .await;
    let text = wos
        .export(&[format!("wos:{UT}")], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["container-title"], "SCIENCE");
    assert_eq!(v[0]["custom"]["ids"]["wos"], UT);
    let d = wos
        .export(&[format!("wos:{UT}")], ExportFormat::Ris)
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end_and_reports_refused_without_a_key() {
    let _slot = with_key(Some(KEY)).await;
    let (mut server, wos) = adapter().await;
    let _s = serve(
        &mut server,
        "/documents",
        Matcher::UrlEncoded("limit".into(), "3".into()),
        200,
        "search",
    )
    .create_async()
    .await;
    let _f = serve(
        &mut server,
        "/documents/WOS%3A000000000000001",
        Matcher::Any,
        200,
        "fetch",
    )
    .create_async()
    .await;
    let registry = Registry::new();
    registry.register(Arc::new(wos));
    let service = DefaultBibSourceService::with_registry(registry);
    let explain = service.explain(None).await;
    assert_eq!(explain.answer.unwrap().sources[0].source, "wos");
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
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.source == "wos" && d.status == Status::Done));
    let r = service.fetch(format!("WOS:{}", &UT[4..]), None, None).await;
    assert_eq!(
        r.answer.unwrap().work.ids["wos"],
        UT,
        "the bare UT form is claimed and fetched"
    );
    // Now with no key: the answer carries the refused diagnostic.
    secrets::install(Arc::new(Holds(None)));
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
    let answer = r.answer.expect("an answer, not an error");
    assert!(answer.rows.is_empty());
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.source == "wos" && d.status == Status::Refused));
}

/// Live, opt-in: `KOINE_SECRET_WOS_API_KEY=… cargo test -p source-wos --
/// --ignored live`. Needs the network and a Starter API key.
#[tokio::test]
#[ignore = "reaches api.clarivate.com and needs KOINE_SECRET_WOS_API_KEY"]
async fn live_search_and_fetch() {
    let wos = Wos::new().with_limiter(Arc::new(Limiter::new()));
    let got = wos
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Abel T"),
                    Query::clause("title", "any", "first star"),
                ],
            },
            page(3, None),
        )
        .await
        .unwrap();
    assert!(!got.rows.is_empty());
    assert!(got.rows.iter().all(|r| r.ids.contains_key("wos")));
    let work = wos
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("Abel 2002 is in the Web of Science");
    assert_eq!(
        work.container_title
            .as_deref()
            .map(str::to_lowercase)
            .as_deref(),
        Some("science")
    );
}
