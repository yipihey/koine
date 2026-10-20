//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches NCBI except the
//! one `#[ignore]` live test at the end.

use std::sync::Arc;
use std::time::Duration;

use koine_sources::kind::Version;
use koine_sources::{Query, Status};
use mockito::{Matcher, Server, ServerGuard};
use source_service::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use source_service::etiquette::Limiter;
use source_service::service::{BibSourceService, DefaultBibSourceService};
use source_service::Registry;

use crate::{PubMed, SOURCE};

const DOI: &str = "10.1126/science.1063991";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The headers of a recorded response the adapter reads.
fn headers(name: &str) -> Vec<(String, String)> {
    fixture(name)
        .lines()
        .filter_map(|l| {
            l.split_once(": ")
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .filter(|(k, _)| k == "x-ratelimit-limit" || k == "retry-after" || k == "content-type")
        .collect()
}

async fn adapter() -> (ServerGuard, PubMed) {
    let server = Server::new_async().await;
    let pubmed = PubMed::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    (server, pubmed)
}

fn serve(server: &mut ServerGuard, path: &str, query: Vec<Matcher>, name: &str) -> mockito::Mock {
    let mut m = server
        .mock("GET", path)
        .match_query(Matcher::AllOf(query))
        .with_status(200)
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
fn the_explain_record_is_valid_and_detect_claims_only_explicit_forms() {
    let record = crate::explain_json();
    assert_eq!(record.source, "pubmed");
    assert_eq!(record.version, crate::API_VERSION);
    for index in [
        "any",
        "title",
        "abstract",
        "author",
        "first_author",
        "year",
        "container",
        "pmid",
        "doi",
        "keyword",
    ] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    assert_eq!(record.indexes["pmid"].exact, Some(true));
    assert_eq!(record.secrets, ["ncbi-api-key"]);
    assert_eq!(record.etiquette.key.as_deref(), Some("optional"));
    assert_eq!(record.page.max, Some(100));
    assert_eq!(record.native, None);
    let p = PubMed::new();
    assert!(p.detect("pmid:11711636"));
    assert!(p.detect("PMID: 11711636"));
    assert!(p.detect("doi:10.1126/science.1063991"));
    // A bare number is not a claim: too loose.
    assert!(!p.detect("11711636"));
    assert!(!p.detect("pmid:abc"));
    assert!(!p.detect("arxiv:2101.00001"));
    assert!(!p.detect("doi:nope"));
}

#[tokio::test]
async fn a_search_is_esearch_then_esummary_and_pages_by_retstart() {
    let (mut server, pubmed) = adapter().await;
    let es = serve(
        &mut server,
        "/esearch.fcgi",
        vec![
            q("db", "pubmed"),
            q("retmode", "json"),
            q(
                "term",
                "(Doudna[Author] AND CRISPR[Title] AND 2014:2014[PDAT])",
            ),
            q("retmax", "3"),
            q("retstart", "0"),
            q("sort", "relevance"),
            q("tool", "koine-source-pubmed"),
            q("email", "tabel@stanford.edu"),
        ],
        "search",
    )
    .match_header(
        "user-agent",
        Matcher::Regex("koine-source-pubmed/.*mailto:tabel@stanford.edu".into()),
    )
    .expect(1)
    .create_async()
    .await;
    let sum = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "25430774,24476820,25468820"), q("retmode", "json")],
        "summary",
    )
    .expect(1)
    .create_async()
    .await;
    let query = Query::And {
        and: vec![
            Query::clause("author", "any", "Doudna"),
            Query::clause("title", "any", "CRISPR"),
            Query::clause("year", "=", "2014"),
        ],
    };
    let got = pubmed.search(&query, page(3, None)).await.unwrap();
    es.assert_async().await;
    sum.assert_async().await;
    assert!(got.diagnostics.is_empty());
    assert_eq!(got.rows.len(), 3);
    assert_eq!(got.next.as_deref(), Some("3"), "3 of 10: the next retstart");
    let row = &got.rows[1];
    assert_eq!(row.ids["pmid"], "24476820");
    assert_eq!(row.ids["pmcid"], "PMC4106473");
    assert_eq!(
        row.title.as_deref(),
        Some("DNA interrogation by the CRISPR RNA-guided endonuclease Cas9")
    );
    assert_eq!(row.author[0].family.as_deref(), Some("Sternberg"));
    assert_eq!(row.author[0].given.as_deref(), Some("SH"));
    assert_eq!(
        row.issued.as_ref().unwrap().date_parts,
        vec![vec![2014, 3, 6]]
    );
    assert_eq!(row.container_title.as_deref(), Some("Nature"));
    assert_eq!(row.volume.as_deref(), Some("507"));
    assert_eq!(row.language.as_deref(), Some("eng"));
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(prov.source, SOURCE);
    assert_eq!(prov.source_id, "24476820");
    assert!(prov.query.as_deref().unwrap().contains("CRISPR"));
    row.validate().unwrap();
    // x-ratelimit-limit: 3 refilled the bucket: one token per 1.4/3 s, no burst.
    assert_eq!(
        pubmed.limiter().limit_of(SOURCE),
        Some((1, Duration::from_millis(466)))
    );
}

#[tokio::test]
async fn a_cursor_is_retstart_and_the_last_page_has_no_next() {
    let (mut server, pubmed) = adapter().await;
    // The recorded page says count 10; asking from 8 with 3 rows → no more.
    let body = fixture("search.json").replace("\"retstart\":\"0\"", "\"retstart\":\"7\"");
    let es = server
        .mock("GET", "/esearch.fcgi")
        .match_query(Matcher::AllOf(vec![q("retstart", "7"), q("retmax", "3")]))
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;
    let _sum = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("retmode", "json")],
        "summary",
    )
    .create_async()
    .await;
    let got = pubmed
        .search(&Query::clause("any", "any", "crispr"), page(3, Some("7")))
        .await
        .unwrap();
    es.assert_async().await;
    assert_eq!(got.rows.len(), 3);
    assert_eq!(got.next, None, "7+3 of count 10 is the end");
}

#[tokio::test]
async fn an_unsupported_clause_is_a_diagnostic_and_a_wholly_unsupported_query_sends_nothing() {
    let (mut server, pubmed) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let got = pubmed
        .search(&Query::clause("arxiv", "=", "2101.00001"), page(5, None))
        .await
        .unwrap();
    m.assert_async().await;
    assert!(got.rows.is_empty());
    assert_eq!(got.diagnostics[0].status, Status::Unsupported);
    assert_eq!(got.diagnostics[0].detail, "bib.arxiv =");
}

#[tokio::test]
async fn an_empty_search_is_no_rows_and_no_summary_call() {
    let (mut server, pubmed) = adapter().await;
    let _es = serve(&mut server, "/esearch.fcgi", vec![], "empty-search")
        .create_async()
        .await;
    let sum = server
        .mock("GET", "/esummary.fcgi")
        .expect(0)
        .create_async()
        .await;
    let got = pubmed
        .search(&Query::clause("title", "any", "zzqxjk"), page(3, None))
        .await
        .unwrap();
    sum.assert_async().await;
    assert!(got.rows.is_empty());
    assert_eq!(got.next, None);
}

#[tokio::test]
async fn a_fetch_by_pmid_parses_the_work_with_its_locations_and_count() {
    let (mut server, pubmed) = adapter().await;
    let m = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "11711636")],
        "fetch",
    )
    .expect(1)
    .create_async()
    .await;
    let work = pubmed.fetch("pmid:11711636").await.unwrap().expect("found");
    m.assert_async().await;
    assert_eq!(work.ty, "article-journal");
    assert_eq!(
        work.title.as_deref(),
        Some("The formation of the first star in the Universe")
    );
    assert_eq!(work.author[0].family.as_deref(), Some("Abel"));
    assert_eq!(work.author[0].given.as_deref(), Some("T"));
    assert_eq!(
        work.issued.as_ref().unwrap().date_parts,
        vec![vec![2002, 1, 4]]
    );
    assert_eq!(work.doi.as_deref(), Some(DOI));
    assert_eq!(work.ids["pmid"], "11711636");
    assert!(!work.ids.contains_key("pmcid"));
    assert_eq!(work.locations.len(), 1);
    let loc = &work.locations[0];
    assert_eq!(loc.version, Version::Published);
    assert!(loc.primary);
    assert_eq!(
        loc.url.as_deref(),
        Some("https://doi.org/10.1126/science.1063991")
    );
    work.validate().unwrap();
}

#[tokio::test]
async fn a_work_in_pmc_gets_a_second_location_with_the_pdf() {
    let (mut server, pubmed) = adapter().await;
    let _m = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "25497837")],
        "fetch-pmc",
    )
    .create_async()
    .await;
    let work = pubmed.fetch("pmid:25497837").await.unwrap().unwrap();
    assert_eq!(work.locations.len(), 2);
    assert!(work.locations[0].primary && !work.locations[1].primary);
    assert_eq!(work.locations[1].version, Version::Accepted);
    assert_eq!(
        work.locations[1].pdf.as_deref(),
        Some("https://pmc.ncbi.nlm.nih.gov/articles/PMC4383097/pdf/")
    );
    let cited = work.metrics.as_ref().unwrap().cited_by.as_ref().unwrap();
    assert_eq!(cited.value, 26);
    assert_eq!(cited.source, "pubmed-pmc", "a PMC-only count says so");
    work.validate().unwrap();
}

#[tokio::test]
async fn a_fetch_by_doi_finds_the_pmid_through_the_article_id_index() {
    let (mut server, pubmed) = adapter().await;
    let lookup = serve(
        &mut server,
        "/esearch.fcgi",
        vec![
            q("term", "\"10.1126/science.1063991\"[AID]"),
            q("retmax", "1"),
        ],
        "doi-lookup",
    )
    .expect(1)
    .create_async()
    .await;
    let _m = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "11711636")],
        "fetch",
    )
    .create_async()
    .await;
    let work = pubmed.fetch(&format!("doi:{DOI}")).await.unwrap().unwrap();
    lookup.assert_async().await;
    assert_eq!(work.ids["pmid"], "11711636");
}

#[tokio::test]
async fn a_fetch_miss_is_none() {
    let (mut server, pubmed) = adapter().await;
    let _m = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "99999999999")],
        "not-found",
    )
    .create_async()
    .await;
    assert_eq!(pubmed.fetch("pmid:99999999999").await.unwrap(), None);
    assert_eq!(pubmed.fetch("arxiv:1").await.unwrap(), None, "not ours");
}

#[tokio::test]
async fn related_cites_and_cited_by_are_elink_then_esummary_of_the_slice() {
    let (mut server, pubmed) = adapter().await;
    let cites = serve(
        &mut server,
        "/elink.fcgi",
        vec![
            q("linkname", "pubmed_pubmed_refs"),
            q("id", "25497837"),
            q("dbfrom", "pubmed"),
        ],
        "cites",
    )
    .expect(1)
    .create_async()
    .await;
    // The slice: the first 3 of 26 references; the summary fixture holds
    // three other records, so only the uids it contains come back.
    let sum = server
        .mock("GET", "/esummary.fcgi")
        .match_query(Matcher::AllOf(vec![q("id", "25357182,24788536,24696461")]))
        .with_status(200)
        .with_body(fixture("summary.json"))
        .expect(1)
        .create_async()
        .await;
    let got = pubmed
        .related("pmid:25497837", Relation::Cites, page(3, None))
        .await
        .unwrap();
    cites.assert_async().await;
    sum.assert_async().await;
    assert_eq!(got.next.as_deref(), Some("3"));
    assert!(got.rows.is_empty(), "the stand-in summary names other uids");

    let cited_by = serve(
        &mut server,
        "/elink.fcgi",
        vec![q("linkname", "pubmed_pubmed_citedin"), q("id", "11711636")],
        "cited-by",
    )
    .expect(1)
    .create_async()
    .await;
    let _s = server
        .mock("GET", "/esummary.fcgi")
        .match_query(Matcher::UrlEncoded("id".into(), "41026823,38730995".into()))
        .with_status(200)
        .with_body(fixture("summary.json"))
        .create_async()
        .await;
    let got = pubmed
        .related("pmid:11711636", Relation::CitedBy, page(2, None))
        .await
        .unwrap();
    cited_by.assert_async().await;
    assert_eq!(got.next.as_deref(), Some("2"), "13 links, 2 shown");

    // A paper with no linked references is an empty page, not an error.
    let _e = serve(
        &mut server,
        "/elink.fcgi",
        vec![q("linkname", "pubmed_pubmed_refs"), q("id", "11711636")],
        "cites-empty",
    )
    .create_async()
    .await;
    let got = pubmed
        .related("pmid:11711636", Relation::Cites, page(5, None))
        .await
        .unwrap();
    assert!(got.rows.is_empty() && got.next.is_none());

    let d = pubmed
        .related("pmid:11711636", Relation::Coreads, PageRequest::default())
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
    assert_eq!(d.detail, "coreads");
}

#[tokio::test]
async fn a_429_backs_off_and_is_never_retried() {
    let (mut server, pubmed) = adapter().await;
    let m = server
        .mock("GET", "/esearch.fcgi")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_header("retry-after", "2")
        .with_body(
            r#"{"error":"API rate limit exceeded","api-key":"1.2.3.4","count":"4","limit":"3"}"#,
        )
        .expect(1)
        .create_async()
        .await;
    let query = Query::clause("any", "any", "crispr");
    let first = pubmed.search(&query, page(2, None)).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 2s"), "{}", first.detail);
    let second = pubmed.search(&query, page(2, None)).await.unwrap_err();
    assert_eq!(second.status, Status::RateLimited);
    assert!(
        second.detail.contains("nothing was sent"),
        "{}",
        second.detail
    );
    m.assert_async().await;
}

#[tokio::test]
async fn eutils_rate_limit_error_in_a_200_is_a_rate_limit_too() {
    let (mut server, pubmed) = adapter().await;
    let _m = server
        .mock("GET", "/esearch.fcgi")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(r#"{"error":"API rate limit exceeded","count":"11"}"#)
        .create_async()
        .await;
    let d = pubmed
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    assert!(pubmed.limiter().backoff_remaining(SOURCE) > Duration::from_secs(1));
}

#[tokio::test]
async fn a_server_error_is_an_error_diagnostic_not_a_panic() {
    let (mut server, pubmed) = adapter().await;
    let _m = server
        .mock("GET", "/esearch.fcgi")
        .match_query(Matcher::Any)
        .with_status(503)
        .with_body("down")
        .create_async()
        .await;
    let d = pubmed
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("503"));
}

#[tokio::test]
async fn the_key_is_sent_as_api_key_raises_the_bucket_and_is_never_logged_in_a_diagnostic() {
    use impress_service_core::secrets::{self, Secret, SecretSource};
    struct Fixed;
    impl SecretSource for Fixed {
        fn get(&self, name: &str) -> Option<Secret> {
            (name == "ncbi-api-key").then(|| Secret::new("test-ncbi-key-123"))
        }
        fn names(&self) -> Vec<String> {
            vec!["ncbi-api-key".into()]
        }
    }
    secrets::install(Arc::new(Fixed));
    let (mut server, pubmed) = adapter().await;
    let m = server
        .mock("GET", "/esearch.fcgi")
        .match_query(Matcher::AllOf(vec![q("api_key", "test-ncbi-key-123")]))
        .with_status(500)
        .with_body("nope")
        .expect(1)
        .create_async()
        .await;
    let d = pubmed
        .search(&Query::clause("any", "any", "x"), page(1, None))
        .await
        .unwrap_err();
    secrets::install(Arc::new(secrets::EnvSource));
    m.assert_async().await;
    assert!(!d.detail.contains("test-ncbi-key"), "{}", d.detail);
    assert_eq!(
        pubmed.limiter().limit_of(SOURCE),
        Some((1, Duration::from_millis(140))),
        "a key is 10 req/s"
    );
}

#[tokio::test]
async fn export_is_csl_json_only() {
    let (mut server, pubmed) = adapter().await;
    let _m = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "11711636")],
        "fetch",
    )
    .create_async()
    .await;
    let text = pubmed
        .export(&["pmid:11711636".into()], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["type"], "article-journal");
    assert_eq!(v[0]["custom"]["ids"]["pmid"], "11711636");
    let d = pubmed
        .export(&["pmid:11711636".into()], ExportFormat::Bibtex)
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end() {
    let (mut server, pubmed) = adapter().await;
    let _a = serve(
        &mut server,
        "/esearch.fcgi",
        vec![q("retmax", "3")],
        "search",
    )
    .create_async()
    .await;
    let _b = server
        .mock("GET", "/esummary.fcgi")
        .match_query(Matcher::UrlEncoded(
            "id".into(),
            "25430774,24476820,25468820".into(),
        ))
        .with_status(200)
        .with_body(fixture("summary.json"))
        .create_async()
        .await;
    let _c = serve(
        &mut server,
        "/esummary.fcgi",
        vec![q("id", "11711636")],
        "fetch",
    )
    .create_async()
    .await;
    let registry = Registry::new();
    registry.register(Arc::new(pubmed));
    let service = DefaultBibSourceService::with_registry(registry);
    let explain = service.explain(None).await;
    assert_eq!(explain.answer.unwrap().sources[0].source, "pubmed");
    let r = service
        .search(
            Query::clause("title", "any", "CRISPR"),
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
        .any(|d| d.source == "pubmed" && d.status == Status::Done));
    assert!(answer.next.unwrap().contains("pubmed"));
    let r = service.fetch("PMID: 11711636".into(), None, None).await;
    assert_eq!(
        r.answer.unwrap().work.title.as_deref(),
        Some("The formation of the first star in the Universe")
    );
}

/// Live, opt-in: `cargo test -p source-pubmed -- --ignored live`. Needs the
/// network; `KOINE_SOURCE_CONTACT` sets `email`, `KOINE_SECRET_NCBI_API_KEY`
/// raises the rate.
#[tokio::test]
#[ignore = "reaches eutils.ncbi.nlm.nih.gov"]
async fn live_search_fetch_and_related() {
    let pubmed = PubMed::new().with_limiter(Arc::new(Limiter::new()));
    let got = pubmed
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Doudna"),
                    Query::clause("title", "any", "CRISPR"),
                ],
            },
            page(3, None),
        )
        .await
        .unwrap();
    assert!(!got.rows.is_empty());
    assert!(got.rows.iter().all(|r| r.ids.contains_key("pmid")));
    let work = pubmed
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("Abel 2002 is in PubMed");
    assert_eq!(work.ids["pmid"], "11711636");
    let cited = pubmed
        .related("pmid:11711636", Relation::CitedBy, page(3, None))
        .await
        .unwrap();
    assert!(!cited.rows.is_empty());
}
