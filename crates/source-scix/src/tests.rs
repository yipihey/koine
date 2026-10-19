//! Tier A over the fixtures (`fixtures/README.md`: hand-written from the
//! documented ADS response shape, except the one recorded 401; no token was
//! reachable). The HTTP layer is a mock server; nothing here reaches
//! api.scixplorer.org except the one `#[ignore]` live test at the end.

use std::sync::Arc;
use std::time::Duration;

use impress_service_core::secrets::{self, Secret, SecretSource};
use koine_sources::kind::Version;
use koine_sources::{Query, Status};
use mockito::{Matcher, Server, ServerGuard};
use source_service::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use source_service::etiquette::Limiter;
use source_service::service::{DefaultSourceService, SourceService};
use source_service::Registry;

use crate::{Scix, SOURCE};

const BIBCODE: &str = "2002Sci...295...93A";
const DOI: &str = "10.1126/science.1063991";
const TOKEN: &str = "test-scix-token-123";

/// The secret slot is process-wide: tests that install one run one at a time.
static SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Holds(Option<&'static str>);
impl SecretSource for Holds {
    fn get(&self, name: &str) -> Option<Secret> {
        (name == "scix-token")
            .then(|| self.0.map(Secret::new))
            .flatten()
    }
    fn names(&self) -> Vec<String> {
        self.0
            .map(|_| vec!["scix-token".to_string()])
            .unwrap_or_default()
    }
}

async fn with_token(token: Option<&'static str>) -> tokio::sync::MutexGuard<'static, ()> {
    let guard = SLOT.lock().await;
    secrets::install(Arc::new(Holds(token)));
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

async fn adapter() -> (ServerGuard, Scix) {
    let server = Server::new_async().await;
    let scix = Scix::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    (server, scix)
}

fn serve(server: &mut ServerGuard, query: Matcher, status: usize, name: &str) -> mockito::Mock {
    let mut m = server
        .mock("GET", "/search/query")
        .match_query(query)
        .with_status(status)
        .with_body(fixture(&format!("{name}.json")));
    for (k, v) in headers(&format!("{name}.headers")) {
        m = m.with_header(&k, &v);
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
fn the_explain_record_is_valid_and_detect_claims_bibcodes_dois_and_arxiv_ids() {
    let record = crate::explain_json();
    assert_eq!(record.source, "scix");
    assert_eq!(record.native.as_deref(), Some("ads-lucene"));
    for index in [
        "any",
        "title",
        "abstract",
        "author",
        "first_author",
        "year",
        "bibcode",
        "doi",
        "arxiv",
        "container",
        "keyword",
    ] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    assert_eq!(record.secrets, ["scix-token"]);
    assert_eq!(record.etiquette.key.as_deref(), Some("required"));
    assert_eq!(record.page.max, Some(100));
    assert_eq!(record.identifiers, ["bibcode", "doi", "arxiv"]);
    let s = Scix::new();
    assert!(s.detect("bibcode:2002Sci...295...93A"));
    assert!(s.detect("bibcode:2020A&A...641A...6P"));
    assert!(s.detect("doi:10.1126/science.1063991"));
    assert!(s.detect("arxiv:astro-ph/0112088"));
    assert!(!s.detect("bibcode:2002"));
    assert!(!s.detect("bibcode:2002Sci...295...93!"));
    assert!(!s.detect("pmid:11711636"));
    // A native query is accepted by the record itself.
    assert!(Query::Native { native: "x".into() }
        .covered_by(&record)
        .is_empty());
}

#[tokio::test]
async fn without_a_token_search_and_fetch_are_refused_and_nothing_is_sent() {
    let _slot = with_token(None).await;
    let (mut server, scix) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let d = scix
        .search(&Query::clause("any", "any", "x"), page(5, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Refused);
    assert!(d.detail.contains("scix-token"), "{}", d.detail);
    assert_eq!(
        scix.fetch(&format!("doi:{DOI}")).await.unwrap_err().status,
        Status::Refused
    );
    m.assert_async().await;
}

#[tokio::test]
async fn a_search_sends_bearer_q_fl_rows_start_sort_and_refills_the_bucket() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let m = serve(
        &mut server,
        Matcher::AllOf(vec![
            q("q", "(author:\"Abel, T\" AND title:(first AND star))"),
            q("fl", crate::parse::FIELDS),
            q("rows", "3"),
            q("start", "0"),
            q("sort", "score desc"),
        ]),
        200,
        "search",
    )
    .match_header("authorization", format!("Bearer {TOKEN}").as_str())
    .match_header(
        "user-agent",
        Matcher::Regex("koine-source-scix/.*mailto:tabel@stanford.edu".into()),
    )
    .expect(1)
    .create_async()
    .await;
    let query = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel, T"),
            Query::clause("title", "any", "first star"),
        ],
    };
    let got = scix.search(&query, page(3, None)).await.unwrap();
    m.assert_async().await;
    assert!(got.diagnostics.is_empty());
    assert_eq!(got.rows.len(), 3);
    assert_eq!(got.next.as_deref(), Some("3"), "3 of 13: the next start");
    let row = &got.rows[0];
    assert_eq!(row.ids["bibcode"], BIBCODE);
    assert_eq!(row.ids["doi"], DOI);
    assert_eq!(row.ids["arxiv"], "astro-ph/0112088");
    assert_eq!(row.ty, "article-journal");
    assert_eq!(
        row.title.as_deref(),
        Some("The Formation of the First Star in the Universe")
    );
    assert_eq!(row.author.len(), 3);
    assert_eq!(row.author[1].family.as_deref(), Some("Bryan"));
    assert_eq!(row.author[1].given.as_deref(), Some("Greg L."));
    assert_eq!(row.issued.as_ref().unwrap().date_parts, vec![vec![2002, 1]]);
    assert_eq!(row.container_title.as_deref(), Some("Science"));
    assert_eq!(row.page.as_deref(), Some("93"));
    assert_eq!(row.issue.as_deref(), Some("5552"));
    assert!(row.abstract_.as_deref().unwrap().starts_with("We describe"));
    let cited = row.metrics.as_ref().unwrap().cited_by.as_ref().unwrap();
    assert_eq!((cited.value, cited.source.as_str()), (1469, "scix"));
    // locations: published (publisher html + pdf), then the arXiv eprint.
    assert_eq!(row.locations.len(), 2);
    let published = &row.locations[0];
    assert!(published.primary);
    assert_eq!(published.version, Version::Published);
    assert_eq!(
        published.url.as_deref(),
        Some("https://scixplorer.org/link_gateway/2002Sci...295...93A/PUB_HTML")
    );
    assert_eq!(
        published.pdf.as_deref(),
        Some("https://scixplorer.org/link_gateway/2002Sci...295...93A/PUB_PDF")
    );
    assert_eq!(published.oa_status.as_deref(), Some("open"));
    let eprint = &row.locations[1];
    assert!(!eprint.primary);
    assert_eq!(eprint.version, Version::Submitted);
    assert_eq!(
        eprint.pdf.as_deref(),
        Some("https://arxiv.org/pdf/astro-ph/0112088")
    );
    // An arXiv-only record: the eprint is the one location, primary.
    let arx = &got.rows[2];
    assert_eq!(arx.ty, "article");
    assert_eq!(arx.locations.len(), 1);
    assert!(arx.locations[0].primary);
    assert_eq!(arx.locations[0].version, Version::Submitted);
    for r in &got.rows {
        r.validate().unwrap();
    }
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(prov.source_id, BIBCODE);
    assert!(prov.query.as_deref().unwrap().contains("first star"));
    // x-ratelimit-limit: 5000 per day.
    assert_eq!(
        scix.limiter().limit_of(SOURCE),
        Some((5000, Duration::from_secs(86400)))
    );
}

#[tokio::test]
async fn a_native_query_passes_through_and_a_cursor_is_the_start() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let m = serve(
        &mut server,
        Matcher::AllOf(vec![
            q("q", "author:\"Abel, T\" year:2002 bibstem:Sci"),
            q("start", "10"),
            q("rows", "100"),
            q("sort", "citation_count desc"),
        ]),
        200,
        "search",
    )
    .expect(1)
    .create_async()
    .await;
    let got = scix
        .search(
            &Query::Native {
                native: "author:\"Abel, T\" year:2002 bibstem:Sci".into(),
            },
            PageRequest {
                limit: 1000,
                cursor: Some("10".into()),
                sort: Some("cited_by".into()),
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    // 10 + 3 of 13: the end.
    assert_eq!(got.next, None);
}

#[tokio::test]
async fn an_unsupported_clause_is_a_diagnostic_and_a_wholly_unsupported_query_sends_nothing() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let got = scix
        .search(&Query::clause("pmid", "=", "1"), page(5, None))
        .await
        .unwrap();
    m.assert_async().await;
    assert!(got.rows.is_empty());
    assert_eq!(got.diagnostics[0].status, Status::Unsupported);
    assert_eq!(got.diagnostics[0].detail, "bib.pmid =");
}

#[tokio::test]
async fn an_empty_search_is_no_rows_and_no_next() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let _m = serve(&mut server, Matcher::Any, 200, "empty-search")
        .create_async()
        .await;
    let got = scix
        .search(&Query::clause("title", "any", "zzqxjk"), page(3, None))
        .await
        .unwrap();
    assert!(got.rows.is_empty() && got.next.is_none());
}

#[tokio::test]
async fn fetch_by_bibcode_doi_and_arxiv_is_a_one_row_search() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    for (id, want) in [
        (format!("bibcode:{BIBCODE}"), format!("bibcode:{BIBCODE}")),
        (format!("doi:{DOI}"), format!("doi:\"{DOI}\"")),
        (
            "arxiv:astro-ph/0112088".to_string(),
            "identifier:\"arXiv:astro-ph/0112088\"".to_string(),
        ),
    ] {
        let m = serve(
            &mut server,
            Matcher::AllOf(vec![q("q", &want), q("rows", "1")]),
            200,
            "fetch",
        )
        .expect(1)
        .create_async()
        .await;
        let work = scix.fetch(&id).await.unwrap().expect("found");
        m.assert_async().await;
        assert_eq!(work.ids["bibcode"], BIBCODE, "{id}");
    }
    assert_eq!(scix.fetch("pmid:1").await.unwrap(), None, "not ours");
    let _e = serve(&mut server, Matcher::Any, 200, "empty-search")
        .create_async()
        .await;
    assert_eq!(
        scix.fetch("bibcode:1999Nope.999..999Z").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn related_wraps_the_id_in_the_ads_operator() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    for (relation, operator) in [
        (Relation::CitedBy, "citations"),
        (Relation::Cites, "references"),
        (Relation::Similar, "similar"),
        (Relation::Coreads, "trending"),
    ] {
        let m = serve(
            &mut server,
            q("q", &format!("{operator}(bibcode:{BIBCODE})")),
            200,
            "citations",
        )
        .expect(1)
        .create_async()
        .await;
        let got = scix
            .related(&format!("bibcode:{BIBCODE}"), relation, page(2, None))
            .await
            .unwrap();
        m.assert_async().await;
        assert_eq!(got.rows.len(), 2);
        assert_eq!(got.next.as_deref(), Some("2"), "2 of 5");
    }
}

#[tokio::test]
async fn a_missing_or_rejected_token_is_refused_and_never_echoed() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    // The real 401 SciX answers without a token (recorded).
    let _m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .with_status(401)
        .with_body(fixture("unauthorized-401.json"))
        .create_async()
        .await;
    let d = scix
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Refused);
    assert!(d.detail.contains("401"));
    assert!(!d.detail.contains("test-scix-token"));
}

#[tokio::test]
async fn a_429_backs_off_and_is_never_retried() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let m = serve(&mut server, Matcher::Any, 429, "rate-limited-429")
        .expect(1)
        .create_async()
        .await;
    let query = Query::clause("any", "any", "stars");
    let first = scix.search(&query, page(2, None)).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 2s"), "{}", first.detail);
    let second = scix.search(&query, page(2, None)).await.unwrap_err();
    assert_eq!(second.status, Status::RateLimited);
    assert!(
        second.detail.contains("nothing was sent"),
        "{}",
        second.detail
    );
    m.assert_async().await;
}

#[tokio::test]
async fn a_spent_daily_quota_backs_off_until_reset_before_the_429_is_earned() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let reset = (chrono::Utc::now().timestamp() + 3600).to_string();
    let m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("x-ratelimit-limit", "5000")
        .with_header("x-ratelimit-remaining", "0")
        .with_header("x-ratelimit-reset", &reset)
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    let query = Query::clause("any", "any", "stars");
    assert_eq!(
        scix.search(&query, page(3, None)).await.unwrap().rows.len(),
        3
    );
    let d = scix.search(&query, page(3, None)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    m.assert_async().await;
    assert!(scix.limiter().backoff_remaining(SOURCE) > Duration::from_secs(3000));
}

#[tokio::test]
async fn a_syntax_error_and_a_server_error_are_error_diagnostics() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let _m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("bad-syntax-400.json"))
        .create_async()
        .await;
    let d = scix
        .search(&Query::Native { native: "(".into() }, page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("unbalanced"), "{}", d.detail);
    let (mut server, scix) = adapter().await;
    let _m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .with_status(503)
        .with_body("down")
        .create_async()
        .await;
    let d = scix
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("503"));
}

#[tokio::test]
async fn export_is_csl_json_only() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let _m = serve(&mut server, Matcher::Any, 200, "fetch")
        .create_async()
        .await;
    let text = scix
        .export(&[format!("bibcode:{BIBCODE}")], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["custom"]["ids"]["bibcode"], BIBCODE);
    assert_eq!(
        scix.export(&[format!("bibcode:{BIBCODE}")], ExportFormat::Bibtex)
            .await
            .unwrap_err()
            .status,
        Status::Unsupported
    );
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end_and_reports_refused_without_a_token() {
    let _slot = with_token(Some(TOKEN)).await;
    let (mut server, scix) = adapter().await;
    let _s = serve(&mut server, q("rows", "3"), 200, "search")
        .create_async()
        .await;
    let _f = serve(&mut server, q("rows", "1"), 200, "fetch")
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(scix));
    let service = DefaultSourceService::with_registry(registry);
    assert_eq!(
        service.explain(None).await.answer.unwrap().sources[0].source,
        "scix"
    );
    let answer = service
        .search(
            Query::clause("title", "any", "first star"),
            None,
            Some(3),
            None,
            None,
            None,
        )
        .await
        .answer
        .expect("ok");
    assert_eq!(answer.rows.len(), 3);
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.source == "scix" && d.status == Status::Done));
    let fetched = service
        .fetch(format!("bibcode:{BIBCODE}"), None, None)
        .await;
    assert_eq!(fetched.answer.unwrap().work.ids["bibcode"], BIBCODE);
    secrets::install(Arc::new(Holds(None)));
    let answer = service
        .search(
            Query::clause("title", "any", "first star"),
            None,
            Some(3),
            None,
            None,
            None,
        )
        .await
        .answer
        .expect("an answer, not an error");
    assert!(answer.rows.is_empty());
    assert!(answer
        .diagnostics
        .iter()
        .any(|d| d.source == "scix" && d.status == Status::Refused));
}

/// Live, opt-in: `KOINE_SECRET_SCIX_TOKEN=… cargo test -p source-scix --
/// --ignored live`. Needs the network and a SciX token.
#[tokio::test]
#[ignore = "reaches api.scixplorer.org and needs KOINE_SECRET_SCIX_TOKEN"]
async fn live_search_fetch_and_related() {
    let scix = Scix::new().with_limiter(Arc::new(Limiter::new()));
    let got = scix
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Abel, T"),
                    Query::clause("year", "=", "2002"),
                    Query::clause("container", "any", "Sci"),
                ],
            },
            page(3, None),
        )
        .await
        .unwrap();
    assert!(got.rows.iter().any(|r| r.ids["bibcode"] == BIBCODE));
    let work = scix
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("Abel 2002 exists");
    assert_eq!(work.ids["bibcode"], BIBCODE);
    let cited = scix
        .related(
            &format!("bibcode:{BIBCODE}"),
            Relation::CitedBy,
            page(3, None),
        )
        .await
        .unwrap();
    assert!(!cited.rows.is_empty());
}
