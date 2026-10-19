//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches api.adsabs.harvard.edu
//! except the one `#[ignore]` live test at the end, which needs the
//! `ads-token` secret.

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

use crate::{Ads, SOURCE};

const BIB: &str = "2002Sci...295...93A";
const TOKEN: &str = "test-token-not-a-secret";

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
        .filter(|(k, _)| k.starts_with("x-ratelimit") || k == "content-type")
        .collect()
}

fn with_headers(m: mockito::Mock, file: &str) -> mockito::Mock {
    let mut m = m;
    for (k, v) in headers(file) {
        m = m.with_header(&k, &v);
    }
    m
}

async fn adapter() -> (ServerGuard, Ads) {
    let server = Server::new_async().await;
    let a = Ads::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()))
        .with_token(Some(TOKEN.into()));
    (server, a)
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort: None,
    }
}

fn search_mock(server: &mut ServerGuard, q: &str, extra: Vec<Matcher>) -> mockito::Mock {
    let mut matchers = vec![Matcher::UrlEncoded("q".into(), q.into())];
    matchers.extend(extra);
    server
        .mock("GET", "/search/query")
        .match_query(Matcher::AllOf(matchers))
        .match_header("authorization", format!("Bearer {TOKEN}").as_str())
}

#[test]
fn the_explain_record_names_the_dialect_the_secret_and_the_indexes() {
    let record = crate::explain_json();
    assert_eq!(record.source, "ads");
    assert_eq!(record.native.as_deref(), Some("ads-lucene"));
    assert_eq!(record.secrets, ["ads-token"]);
    assert_eq!(record.page.max, Some(100));
    assert_eq!(record.etiquette.limit.as_deref(), Some("headers"));
    for i in [
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
        "cites",
        "cited_by",
        "similar_to",
    ] {
        assert!(record.indexes.contains_key(i), "{i}");
    }
    assert!(record.operations.iter().any(|o| o == "related"));
}

#[test]
fn detect_claims_bibcodes_dois_and_arxiv_ids() {
    let a = Ads::new();
    for yes in [
        "bibcode:2002Sci...295...93A",
        "bibcode:2015NJPh...17g5013H",
        "bibcode:2009ApJ...701L.133A",
        "bibcode:2006Ap&SS.302..225L",
        "bibcode:2017arXiv170603762V",
        "bibcode:2001astro.ph.12088A",
        "doi:10.1126/science.1063991",
        "arxiv:1706.03762",
        "arxiv:astro-ph/0112088",
    ] {
        assert!(a.detect(yes), "{yes}");
    }
    for no in [
        "bibcode:nope",
        "bibcode:2002Sci...295...93",
        "pmid:11711636",
        "openalex:W1",
        "2002Sci...295...93A",
    ] {
        assert!(!a.detect(no), "{no}");
    }
}

#[tokio::test]
async fn a_search_sends_lucene_rows_start_sort_fl_the_bearer_and_the_contact() {
    let (mut server, ads) = adapter().await;
    let m = search_mock(
        &mut server,
        "(author:\"Abel\" AND title:\"first star\" AND year:2002)",
        vec![
            Matcher::UrlEncoded("rows".into(), "3".into()),
            Matcher::UrlEncoded("start".into(), "0".into()),
            Matcher::UrlEncoded("sort".into(), "score desc".into()),
            Matcher::UrlEncoded("fl".into(), crate::parse::FIELDS.into()),
        ],
    )
    .match_header(
        "user-agent",
        Matcher::Regex("koine-source-ads/.*mailto:tabel@stanford.edu".into()),
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
            Query::clause("title", "adj", "first star"),
            Query::clause("year", "=", "2002"),
        ],
    };
    let p = ads
        .search(
            &q,
            PageRequest {
                limit: 3,
                cursor: None,
                sort: Some("relevance".into()),
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert_eq!(p.rows.len(), 2);
    assert_eq!(p.next, None, "numFound 2, both in hand");
    assert!(p.diagnostics.is_empty());
    let row = &p.rows[0];
    assert_eq!(row.ids["bibcode"], BIB);
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(
        (prov.source.as_str(), prov.source_id.as_str()),
        (SOURCE, BIB)
    );
    assert!(prov.query.as_deref().unwrap().contains("first star"));
    for r in &p.rows {
        r.validate().unwrap();
    }
    // The conference paper: no DOI of arXiv's, a proceedings type, a pubdate
    // with neither month nor day.
    let conf = &p.rows[1];
    assert_eq!(conf.ty, "paper-conference");
    assert_eq!(conf.doi.as_deref(), Some("10.1109/VISUAL.2002.1183824"));
    assert_eq!(conf.issued.as_ref().unwrap().date_parts, vec![vec![2002]]);
}

#[tokio::test]
async fn offset_paging_native_passthrough_and_the_row_cap() {
    let (mut server, ads) = adapter().await;
    let m = search_mock(
        &mut server,
        "(author:\"Abel, T\" AND year:[2000 TO 2003])",
        vec![
            Matcher::UrlEncoded("rows".into(), "2".into()),
            Matcher::UrlEncoded("start".into(), "0".into()),
        ],
    )
    .with_status(200)
    .with_body(fixture("search-more.json"))
    .expect(1)
    .create_async()
    .await;
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel, T"),
            Query::clause("year", "within", "2000 2003"),
        ],
    };
    let p = ads.search(&q, page(2)).await.unwrap();
    m.assert_async().await;
    assert_eq!(p.rows.len(), 2);
    assert_eq!(p.next.as_deref(), Some("2"), "2 of numFound 52");

    // The next page asks start=2; a huge limit is capped at 100.
    let m2 = search_mock(
        &mut server,
        "(author:\"Abel, T\" AND year:[2000 TO 2003])",
        vec![
            Matcher::UrlEncoded("rows".into(), "100".into()),
            Matcher::UrlEncoded("start".into(), "2".into()),
        ],
    )
    .with_status(200)
    .with_body(fixture("search-more.json"))
    .expect(1)
    .create_async()
    .await;
    let p = ads
        .search(
            &q,
            PageRequest {
                limit: 5000,
                cursor: p.next,
                sort: None,
            },
        )
        .await
        .unwrap();
    m2.assert_async().await;
    assert_eq!(p.next.as_deref(), Some("4"));

    // A native query passes through untouched (parenthesised).
    let m3 = search_mock(
        &mut server,
        "(property:refereed author:\"^Abel, T\")",
        vec![],
    )
    .with_status(200)
    .with_body(fixture("search.json"))
    .expect(1)
    .create_async()
    .await;
    ads.search(
        &Query::Native {
            native: "property:refereed author:\"^Abel, T\"".into(),
        },
        page(3),
    )
    .await
    .unwrap();
    m3.assert_async().await;
}

#[tokio::test]
async fn an_unsupported_clause_is_a_diagnostic_and_a_wholly_unsupported_query_sends_nothing() {
    let (mut server, ads) = adapter().await;
    let m = server
        .mock("GET", "/search/query")
        .expect(0)
        .create_async()
        .await;
    let p = ads
        .search(&Query::clause("issn", "=", "0036-8075"), page(5))
        .await
        .unwrap();
    m.assert_async().await;
    assert!(p.rows.is_empty());
    assert_eq!(p.diagnostics[0].status, Status::Unsupported);
    assert_eq!(p.diagnostics[0].detail, "bib.issn =");
}

#[tokio::test]
async fn a_fetch_by_bibcode_maps_every_field_of_the_work_kind() {
    let (mut server, ads) = adapter().await;
    let m = search_mock(
        &mut server,
        &format!("bibcode:\"{BIB}\""),
        vec![Matcher::UrlEncoded("rows".into(), "1".into())],
    )
    .with_status(200)
    .with_body(fixture("fetch.json"));
    let m = with_headers(m, "fetch.headers").create_async().await;
    let w = ads
        .fetch(&format!("bibcode:{BIB}"))
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
    assert_eq!(w.author[2].family.as_deref(), Some("Norman"));
    assert_eq!(w.author[2].orcid, None, "ADS's `-` is no ORCID");
    assert_eq!(w.issued.as_ref().unwrap().date_parts, vec![vec![2002, 1]]);
    assert_eq!(w.container_title.as_deref(), Some("Science"));
    assert_eq!(w.volume.as_deref(), Some("295"));
    assert_eq!(w.issue.as_deref(), Some("5552"));
    assert_eq!(w.page.as_deref(), Some("93-98"), "page_range");
    let abs = w.abstract_.as_deref().unwrap();
    assert!(abs.starts_with("We describe results"));
    assert!(abs.contains("100 Msolar"), "SUB markup stripped: {abs}");
    assert_eq!(
        w.doi.as_deref(),
        Some("10.1126/science.1063991"),
        "not arXiv's DOI"
    );
    assert_eq!(w.ids["bibcode"], BIB);
    assert_eq!(w.ids["doi"], "10.1126/science.1063991");
    assert_eq!(w.ids["arxiv"], "astro-ph/0112088");
    assert_eq!(
        w.url.as_deref(),
        Some("https://ui.adsabs.harvard.edu/abs/2002Sci...295...93A/abstract")
    );
    let cited = w.metrics.as_ref().unwrap().cited_by.as_ref().unwrap();
    assert_eq!((cited.value, cited.source.as_str()), (1469, SOURCE));
    // esources PUB_HTML + EPRINT_HTML/PDF, property EPRINT_OPENACCESS.
    assert_eq!(w.locations.len(), 2);
    let published = &w.locations[0];
    assert!(published.primary);
    assert_eq!(published.version, Version::Published);
    assert_eq!(
        published.url.as_deref(),
        Some("https://ui.adsabs.harvard.edu/link_gateway/2002Sci...295...93A/PUB_HTML")
    );
    assert_eq!(published.pdf, None, "no PUB_PDF esource");
    let eprint = &w.locations[1];
    assert!(!eprint.primary);
    assert_eq!(eprint.version, Version::Submitted);
    assert_eq!(
        eprint.pdf.as_deref(),
        Some("https://ui.adsabs.harvard.edu/link_gateway/2002Sci...295...93A/EPRINT_PDF")
    );
    assert_eq!(eprint.oa_status.as_deref(), Some("green"));
    w.validate().unwrap();
    assert_eq!(w.to_csl_json()["custom"]["ids"]["bibcode"], json!(BIB));
}

#[tokio::test]
async fn an_eprint_is_a_submitted_location_with_no_container_or_page_and_a_miss_is_none() {
    let (mut server, ads) = adapter().await;
    let _e = search_mock(&mut server, "identifier:\"arXiv:1706.03762\"", vec![])
        .with_status(200)
        .with_body(fixture("fetch-eprint.json"))
        .create_async()
        .await;
    let w = ads.fetch("arxiv:1706.03762").await.unwrap().unwrap();
    assert_eq!(w.ids["bibcode"], "2017arXiv170603762V");
    assert_eq!(w.ids["arxiv"], "1706.03762");
    assert_eq!(w.doi, None, "arXiv's own DOI is not the work's");
    assert_eq!(w.ty, "article");
    assert_eq!(w.container_title, None);
    assert_eq!(w.page, None);
    assert_eq!(w.locations.len(), 1);
    assert_eq!(w.locations[0].version, Version::Submitted);
    assert!(w.locations[0].primary);
    assert_eq!(w.locations[0].oa_status.as_deref(), Some("green"));
    w.validate().unwrap();

    let _m = search_mock(&mut server, "bibcode:\"1900Zzz...999..999Z\"", vec![])
        .with_status(200)
        .with_body(fixture("fetch-miss.json"))
        .create_async()
        .await;
    assert_eq!(
        ads.fetch("bibcode:1900Zzz...999..999Z").await.unwrap(),
        None
    );
    assert_eq!(ads.fetch("pmid:1").await.unwrap(), None, "not ours");
}

#[tokio::test]
async fn related_runs_the_ads_operators_and_pages_by_offset() {
    let (mut server, ads) = adapter().await;
    let id = format!("bibcode:{BIB}");
    for (relation, op, file, first) in [
        (
            Relation::Cites,
            "references",
            "references.json",
            "1980ApJ...239..968N",
        ),
        (
            Relation::CitedBy,
            "citations",
            "citations.json",
            "2015NJPh...17g5013H",
        ),
        (
            Relation::Similar,
            "similar",
            "similar.json",
            "2014ApJ...781...60H",
        ),
        (
            Relation::Coreads,
            "trending",
            "coreads.json",
            "2002Sci...295...93A",
        ),
    ] {
        let m = search_mock(
            &mut server,
            &format!("{op}(bibcode:\"{BIB}\")"),
            vec![
                Matcher::UrlEncoded("rows".into(), "2".into()),
                Matcher::UrlEncoded("start".into(), "0".into()),
            ],
        )
        .with_status(200)
        .with_body(fixture(file))
        .expect(1)
        .create_async()
        .await;
        let p = ads.related(&id, relation, page(2)).await.unwrap();
        m.assert_async().await;
        assert_eq!(p.rows.len(), 2, "{op}");
        assert_eq!(p.rows[0].ids["bibcode"], first, "{op}");
        assert_eq!(p.next.as_deref(), Some("2"), "{op}: more than 2 found");
    }
    // A DOI is an id ADS can start from.
    let m = search_mock(
        &mut server,
        "citations(doi:\"10.1126/science.1063991\")",
        vec![],
    )
    .with_status(200)
    .with_body(fixture("citations.json"))
    .expect(1)
    .create_async()
    .await;
    ads.related("doi:10.1126/science.1063991", Relation::CitedBy, page(2))
        .await
        .unwrap();
    m.assert_async().await;
}

#[tokio::test]
async fn export_posts_bibcodes_resolves_a_doi_first_and_csl_json_is_native() {
    let (mut server, ads) = adapter().await;
    let bib = server
        .mock("POST", "/export/bibtex")
        .match_header("authorization", format!("Bearer {TOKEN}").as_str())
        .match_body(Matcher::Json(json!({"bibcode": [BIB]})))
        .with_status(200)
        .with_body(fixture("export-bibtex.json"))
        .expect(1)
        .create_async()
        .await;
    let text = ads
        .export(&[format!("bibcode:{BIB}")], ExportFormat::Bibtex)
        .await
        .unwrap();
    bib.assert_async().await;
    assert!(text.starts_with("@ARTICLE{2002Sci...295...93A"), "{text}");

    let ris = server
        .mock("POST", "/export/ris")
        .match_body(Matcher::Json(json!({"bibcode": [BIB]})))
        .with_status(200)
        .with_body(fixture("export-ris.json"))
        .expect(1)
        .create_async()
        .await;
    let _resolve = search_mock(&mut server, "doi:\"10.1126/science.1063991\"", vec![])
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let text = ads
        .export(
            &["doi:10.1126/science.1063991".to_string()],
            ExportFormat::Ris,
        )
        .await
        .unwrap();
    ris.assert_async().await;
    assert!(text.starts_with("TY  - JOUR"), "{text}");

    let v: Value = serde_json::from_str(
        &ads.export(
            &["doi:10.1126/science.1063991".to_string()],
            ExportFormat::CslJson,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(v[0]["container-title"], json!("Science"));
    assert_eq!(v[0]["custom"]["ids"]["bibcode"], json!(BIB));
}

#[tokio::test]
async fn a_rejected_token_is_refused_and_a_bad_query_is_an_error_with_adss_message() {
    let (mut server, ads) = adapter().await;
    let _u = server
        .mock("GET", "/search/query")
        .match_query(Matcher::UrlEncoded("q".into(), "title:x".into()))
        .with_status(401)
        .with_body(fixture("unauthorized.json"))
        .create_async()
        .await;
    let d = ads
        .search(&Query::clause("title", "any", "x"), page(1))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Refused);
    assert!(d.detail.contains("ads-token"), "{}", d.detail);

    let _s = server
        .mock("GET", "/search/query")
        .match_query(Matcher::UrlEncoded("q".into(), "(author:()".into()))
        .with_status(400)
        .with_body(fixture("syntax-error.json"))
        .create_async()
        .await;
    let d = ads
        .search(
            &Query::Native {
                native: "author:(".into(),
            },
            page(1),
        )
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("http 400"), "{}", d.detail);
    assert!(d.detail.contains("Syntax Error"), "{}", d.detail);
}

#[tokio::test]
async fn a_429_backs_off_and_an_exhausted_quota_waits_for_the_reset_epoch() {
    let (mut server, ads) = adapter().await;
    let m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_header("retry-after", "5")
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "stars");
    let first = ads.search(&q, page(1)).await.unwrap_err();
    assert_eq!(first.status, Status::RateLimited);
    assert!(first.detail.contains("backing off 5s"), "{}", first.detail);
    let second = ads.search(&q, page(1)).await.unwrap_err();
    assert!(second.detail.contains("nothing was sent"));
    m.assert_async().await;

    // Remaining 0 with a reset two minutes out: this answer is fine, the next waits.
    let (mut server, ads) = adapter().await;
    let reset = (chrono::Utc::now().timestamp() + 120).to_string();
    let _m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("x-ratelimit-remaining", "0")
        .with_header("x-ratelimit-reset", &reset)
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    assert!(ads.search(&q, page(2)).await.is_ok());
    let left = ads.limiter().backoff_remaining(SOURCE);
    assert!(
        left > Duration::from_secs(100) && left <= Duration::from_secs(120),
        "{left:?}"
    );
    assert_eq!(
        ads.search(&q, page(2)).await.unwrap_err().status,
        Status::RateLimited
    );
}

#[tokio::test]
async fn the_token_is_read_from_the_secret_slot_and_absent_is_refused_without_a_request() {
    use impress_service_core::secrets::{self, Secret, SecretSource};
    use std::sync::Mutex;
    struct Slot(Mutex<Option<String>>);
    impl SecretSource for Slot {
        fn get(&self, name: &str) -> Option<Secret> {
            (name == "ads-token")
                .then(|| self.0.lock().unwrap().clone().map(Secret::new))
                .flatten()
        }
        fn names(&self) -> Vec<String> {
            Vec::new()
        }
    }
    let slot = Arc::new(Slot(Mutex::new(None)));
    secrets::install(slot.clone());

    let mut server = Server::new_async().await;
    // No `with_token`: this adapter reads the slot.
    let ads = Ads::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()));
    let none = server
        .mock("GET", "/search/query")
        .expect(0)
        .create_async()
        .await;
    let d = ads
        .search(&Query::clause("title", "any", "stars"), page(1))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Refused);
    assert!(d.detail.contains("KOINE_SECRET_ADS_TOKEN"), "{}", d.detail);
    none.assert_async().await;
    none.remove_async().await;

    *slot.0.lock().unwrap() = Some("from-the-slot".into());
    let m = server
        .mock("GET", "/search/query")
        .match_query(Matcher::Any)
        .match_header("authorization", "Bearer from-the-slot")
        .with_status(200)
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    assert!(ads
        .search(&Query::clause("title", "any", "stars"), page(2))
        .await
        .is_ok());
    m.assert_async().await;
    secrets::install(Arc::new(secrets::EnvSource));
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end() {
    let (mut server, ads) = adapter().await;
    let _s = search_mock(&mut server, "title:stars", vec![])
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let _f = search_mock(&mut server, &format!("bibcode:\"{BIB}\""), vec![])
        .with_status(200)
        .with_body(fixture("fetch.json"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(ads));
    let service = DefaultSourceService::with_registry(registry);
    let r = service
        .search(
            Query::clause("title", "any", "stars"),
            None,
            Some(3),
            None,
            None,
        )
        .await;
    assert_eq!(r.answer.expect("ok").rows.len(), 2);
    let r = service
        .fetch(format!("https://ui.adsabs.harvard.edu/abs/{BIB}"), None)
        .await;
    assert_eq!(r.answer.unwrap().work.ids["bibcode"], BIB);
}

/// Live, opt-in: `KOINE_SECRET_ADS_TOKEN=… cargo test -p source-ads --
/// --ignored live`. Needs the network and an ADS token.
#[tokio::test]
#[ignore = "reaches api.adsabs.harvard.edu and needs the ads-token secret"]
async fn live_search_fetch_and_related() {
    let ads = Ads::new().with_limiter(Arc::new(Limiter::new()));
    let p = ads
        .search(
            &Query::And {
                and: vec![
                    Query::clause("author", "any", "Abel, T"),
                    Query::clause("title", "adj", "first star"),
                ],
            },
            page(3),
        )
        .await
        .unwrap();
    assert!(!p.rows.is_empty());
    assert!(p.rows.iter().all(|r| r.ids.contains_key("bibcode")));
    let w = ads
        .fetch(&format!("bibcode:{BIB}"))
        .await
        .unwrap()
        .expect("Abel 2002 exists");
    assert_eq!(w.doi.as_deref(), Some("10.1126/science.1063991"));
    let refs = ads
        .related(&format!("bibcode:{BIB}"), Relation::Cites, page(2))
        .await
        .unwrap();
    assert_eq!(refs.rows.len(), 2);
}
