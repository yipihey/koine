//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches
//! api.semanticscholar.org except the one `#[ignore]` live test at the end.

use std::sync::Arc;
use std::time::Duration;

use koine_sources::kind::Version;
use koine_sources::{Query, Status};
use mockito::{Matcher, Server, ServerGuard};
use serde_json::{json, Value};
use source_service::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use source_service::etiquette::Limiter;
use source_service::service::{BibSourceService, DefaultBibSourceService};
use source_service::Registry;

use crate::{SemanticScholar, FIELDS, LIST_FIELDS, SOURCE};

const TRANSFORMER: &str = "204e3073870fae3d05bcbc2f6a8e263d9b72e776";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

async fn adapter() -> (ServerGuard, SemanticScholar) {
    let server = Server::new_async().await;
    let s2 = SemanticScholar::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    // The 1/s bucket is asserted once, in its own test; the rest do not wait.
    s2.limiter().set_limit(SOURCE, 1000, Duration::from_secs(1));
    (server, s2)
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort: None,
    }
}

#[test]
fn the_explain_record_is_valid_and_detect_claims_the_ids_s2_resolves() {
    let record = crate::explain_json();
    assert_eq!(record.source, "s2");
    assert_eq!(record.secrets, ["s2-api-key"]);
    assert_eq!(record.etiquette.key.as_deref(), Some("optional"));
    assert_eq!(record.page.max, Some(100));
    for index in [
        "any",
        "title",
        "year",
        "doi",
        "arxiv",
        "pmid",
        "cites",
        "cited_by",
        "similar_to",
    ] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    assert!(
        !record.indexes.contains_key("author"),
        "S2 search has no author field"
    );
    let a = SemanticScholar::new();
    assert!(a.detect("doi:10.18653/v1/N19-1423"));
    assert!(a.detect("arxiv:1706.03762"));
    assert!(a.detect("pmid:11729294"));
    assert!(a.detect("corpusid:13756489"));
    assert!(a.detect(&format!("s2:{TRANSFORMER}")));
    assert!(!a.detect("s2:not-a-paper-id"));
    assert!(!a.detect("corpusid:abc"));
    assert!(!a.detect("bibcode:2002Sci...295...93A"));
    assert!(!a.detect("dblp:conf/nips/VaswaniSPUJGKP17"));
}

#[tokio::test]
async fn a_search_sends_query_fields_offset_limit_and_parses_the_transformer() {
    let (mut server, s2) = adapter().await;
    let m = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query".into(), "attention is all you need".into()),
            Matcher::UrlEncoded("fields".into(), FIELDS.into()),
            Matcher::UrlEncoded("offset".into(), "0".into()),
            Matcher::UrlEncoded("limit".into(), "3".into()),
        ]))
        .match_header(
            "user-agent",
            Matcher::Regex("koine-source-s2/.*mailto:tabel@stanford.edu".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    let out = s2
        .search(
            &Query::clause("title", "any", "attention is all you need"),
            page(3),
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert!(out.diagnostics.is_empty());
    assert_eq!(out.rows.len(), 3);
    assert_eq!(out.next.as_deref(), Some("3"));
    let t = &out.rows[0];
    t.validate().unwrap();
    assert_eq!(t.title.as_deref(), Some("Attention is All you Need"));
    assert_eq!(
        t.ty, "paper-conference",
        "Conference wins over JournalArticle"
    );
    assert_eq!(t.ids["s2"], TRANSFORMER);
    assert_eq!(t.ids["arxiv"], "1706.03762");
    assert_eq!(t.ids["mag"], "2626778328");
    assert_eq!(t.ids["dblp"], "journals/corr/VaswaniSPUJGKP17");
    assert!(!t.ids.contains_key("doi"));
    assert!(!t.ids.contains_key("corpusid"));
    assert_eq!(
        t.container_title.as_deref(),
        Some("Neural Information Processing Systems")
    );
    assert_eq!(t.page.as_deref(), Some("5998-6008"));
    assert_eq!(
        t.issued.as_ref().unwrap().date_parts,
        vec![vec![2017, 6, 12]]
    );
    assert_eq!(
        t.metrics.as_ref().unwrap().cited_by.as_ref().unwrap().value,
        197441
    );
    assert_eq!(t.author.len(), 8);
    assert_eq!(t.author[0].family.as_deref(), Some("Vaswani"));
    assert_eq!(t.author[0].given.as_deref(), Some("Ashish"));
    assert_eq!(t.author[2].family.as_deref(), Some("Parmar"));
    assert_eq!(t.author[2].given.as_deref(), Some("N."));
    // No DOI and an empty openAccessPdf url: the one location is the arXiv copy.
    assert_eq!(t.locations.len(), 1);
    assert_eq!(t.locations[0].version, Version::Submitted);
    assert_eq!(
        t.locations[0].pdf.as_deref(),
        Some("https://arxiv.org/pdf/1706.03762")
    );
    assert!(t.locations[0].primary);
    let prov = t.provenance.as_ref().unwrap();
    assert_eq!(
        (prov.source.as_str(), prov.source_id.as_str()),
        (SOURCE, TRANSFORMER)
    );
    for r in &out.rows {
        r.validate().unwrap();
    }
}

#[test]
fn the_documented_keyless_rate_is_declared_to_the_limiter() {
    let s2 = SemanticScholar::new().with_limiter(Arc::new(Limiter::new()));
    assert_eq!(
        s2.limiter().limit_of(SOURCE),
        Some((1, Duration::from_secs(1)))
    );
}

#[tokio::test]
async fn a_year_range_is_a_year_parameter_and_open_access_versions_follow_the_rule() {
    let (mut server, s2) = adapter().await;
    let m = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("query".into(), "first stars universe".into()),
            Matcher::UrlEncoded("year".into(), "2002-2003".into()),
            Matcher::UrlEncoded("limit".into(), "2".into()),
        ]))
        .with_status(200)
        .with_body(fixture("year-search.json"))
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("any", "any", "first stars universe"),
            Query::clause("year", "within", "2002 2003"),
        ],
    };
    let out = s2.search(&q, page(2)).await.unwrap();
    m.assert_async().await;
    assert_eq!(out.rows.len(), 2);
    let review = &out.rows[0];
    assert_eq!(review.doi.as_deref(), Some("10.1143/PTPS.147.11"));
    assert_eq!(review.ty, "article-journal");
    assert_eq!(review.volume.as_deref(), Some("147"));
    // DOI = published + primary; the BRONZE publisher pdf = published, not primary.
    assert_eq!(
        review.locations[0].url.as_deref(),
        Some("https://doi.org/10.1143/PTPS.147.11")
    );
    assert!(review.locations[0].primary);
    assert_eq!(review.locations[1].version, Version::Published);
    assert_eq!(review.locations[1].oa_status.as_deref(), Some("bronze"));
    assert!(!review.locations[1].primary);
    assert!(review.locations[1]
        .pdf
        .as_deref()
        .unwrap()
        .ends_with(".pdf"));
    // Second paper: BRONZE pdf at aanda.org, plus an arXiv id with no arxiv location yet.
    let a = &out.rows[1];
    assert!(a.locations.iter().any(|l| l.version == Version::Submitted
        && l.url.as_deref() == Some("https://arxiv.org/abs/astro-ph/0311082")));
    assert_eq!(a.locations.iter().filter(|l| l.primary).count(), 1);
}

#[test]
fn the_version_rule_and_the_author_split() {
    use crate::parse::{author, oa_version};
    assert_eq!(
        oa_version("https://arxiv.org/pdf/1706.03762", Some("GREEN")),
        Version::Submitted
    );
    assert_eq!(
        oa_version("https://www.biorxiv.org/content/x.pdf", None),
        Version::Submitted
    );
    assert_eq!(
        oa_version("https://europepmc.org/articles/PMC1", Some("GREEN")),
        Version::Submitted
    );
    assert_eq!(
        oa_version("https://repo.example.edu/x.pdf", Some("GREEN")),
        Version::Accepted
    );
    assert_eq!(
        oa_version("https://doi.org/10.1/x", Some("HYBRID")),
        Version::Published
    );
    assert_eq!(
        oa_version("https://pub.example.com/x.pdf", Some("GOLD")),
        Version::Published
    );

    let a = author("Ludwig van Beethoven");
    assert_eq!(
        (a.family.as_deref(), a.given.as_deref()),
        (Some("van Beethoven"), Some("Ludwig"))
    );
    let a = author("Martin Luther King Jr.");
    assert_eq!(
        (a.family.as_deref(), a.given.as_deref()),
        (Some("King Jr."), Some("Martin Luther"))
    );
    let a = author("Abel, Tom");
    assert_eq!(
        (a.family.as_deref(), a.given.as_deref()),
        (Some("Abel"), Some("Tom"))
    );
    let a = author("Aidan N. Gomez");
    assert_eq!(
        (a.family.as_deref(), a.given.as_deref()),
        (Some("Gomez"), Some("Aidan N."))
    );
    let a = author("Plato");
    assert_eq!(
        (a.family.as_deref(), a.given.as_deref()),
        (Some("Plato"), None)
    );
    let a = author("Jr. Smith");
    assert_eq!(a.family.as_deref(), Some("Smith"));
}

#[tokio::test]
async fn unsupported_clauses_are_diagnostics_a_wholly_unsupported_query_and_the_window_send_nothing(
) {
    let (mut server, s2) = adapter().await;
    let nothing = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let out = s2
        .search(&Query::clause("author", "any", "Vaswani"), page(5))
        .await
        .unwrap();
    assert!(out.rows.is_empty());
    assert_eq!(out.diagnostics[0].status, Status::Unsupported);
    assert_eq!(out.diagnostics[0].detail, "bib.author any");
    let out = s2
        .search(
            &Query::clause("any", "any", "dark matter"),
            PageRequest {
                limit: 100,
                cursor: Some("950".into()),
                sort: None,
            },
        )
        .await
        .unwrap();
    assert!(out.rows.is_empty());
    assert!(out.diagnostics[0].detail.contains("1000"));
    nothing.assert_async().await;
}

#[tokio::test]
async fn fetch_by_doi_arxiv_corpusid_and_s2_id_uses_s2s_spelling_and_a_404_is_none() {
    let (mut server, s2) = adapter().await;
    let doi = server
        .mock("GET", "/graph/v1/paper/DOI:10.18653/v1/N19-1423")
        .match_query(Matcher::UrlEncoded("fields".into(), FIELDS.into()))
        .with_status(200)
        .with_body(fixture("fetch-doi.json"))
        .create_async()
        .await;
    let arxiv = server
        .mock("GET", "/graph/v1/paper/ARXIV:1706.03762")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-arxiv.json"))
        .create_async()
        .await;
    let corpus = server
        .mock("GET", "/graph/v1/paper/CorpusId:13756489")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-corpusid.json"))
        .create_async()
        .await;
    let by_id = server
        .mock("GET", &format!("/graph/v1/paper/{TRANSFORMER}")[..])
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-arxiv.json"))
        .create_async()
        .await;
    let missing = server
        .mock("GET", "/graph/v1/paper/DOI:10.1126/science.1063991")
        .match_query(Matcher::Any)
        .with_status(404)
        .with_body(fixture("not-found.json"))
        .create_async()
        .await;

    let bert = s2.fetch("doi:10.18653/v1/N19-1423").await.unwrap().unwrap();
    doi.assert_async().await;
    bert.validate().unwrap();
    assert_eq!(
        bert.title.as_deref(),
        Some("BERT: Pre-training of Deep Bidirectional Transformers for Language Understanding")
    );
    assert_eq!(bert.ids["doi"], "10.18653/v1/N19-1423");
    assert_eq!(bert.ids["arxiv"], "1810.04805");
    assert_eq!(bert.ids["dblp"], "journals/corr/abs-1810-04805");
    // DOI first and primary; the HYBRID doi.org openAccessPdf is that same
    // landing page (in lower case), so it adds its licence there.
    assert_eq!(
        bert.locations[0].url.as_deref(),
        Some("https://doi.org/10.18653/v1/N19-1423")
    );
    assert_eq!(bert.locations[0].license.as_deref(), Some("CCBY"));
    assert_eq!(bert.locations[0].oa_status.as_deref(), Some("hybrid"));
    assert_eq!(bert.locations[0].version, Version::Published);
    assert_eq!(bert.locations.len(), 2, "doi + arxiv");
    assert_eq!(bert.locations[1].version, Version::Submitted);
    assert_eq!(bert.author[0].family.as_deref(), Some("Devlin"));
    assert_eq!(bert.author[0].given.as_deref(), Some("J."));

    let a = s2.fetch("arxiv:1706.03762v7").await.unwrap().unwrap();
    arxiv.assert_async().await;
    let c = s2.fetch("corpusid:13756489").await.unwrap().unwrap();
    corpus.assert_async().await;
    let i = s2
        .fetch(&format!("s2:{TRANSFORMER}"))
        .await
        .unwrap()
        .unwrap();
    by_id.assert_async().await;
    assert_eq!(a.ids["s2"], TRANSFORMER);
    assert_eq!(c.ids, a.ids);
    assert_eq!(i.ids, a.ids);

    assert_eq!(s2.fetch("doi:10.1126/science.1063991").await.unwrap(), None);
    missing.assert_async().await;
    assert_eq!(s2.fetch("bibcode:2002Sci...295...93A").await.unwrap(), None);
}

#[tokio::test]
async fn related_cites_are_references_cited_by_are_citations_and_pages_follow_next() {
    let (mut server, s2) = adapter().await;
    let refs = server
        .mock("GET", "/graph/v1/paper/ARXIV:1706.03762/references")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("fields".into(), LIST_FIELDS.into()),
            Matcher::UrlEncoded("offset".into(), "0".into()),
            Matcher::UrlEncoded("limit".into(), "2".into()),
        ]))
        .with_status(200)
        .with_body(fixture("references.json"))
        .create_async()
        .await;
    let cites = server
        .mock("GET", "/graph/v1/paper/ARXIV:1706.03762/citations")
        .match_query(Matcher::UrlEncoded("offset".into(), "4".into()))
        .with_status(200)
        .with_body(fixture("citations.json"))
        .create_async()
        .await;
    let out = s2
        .related("arxiv:1706.03762", Relation::Cites, page(2))
        .await
        .unwrap();
    refs.assert_async().await;
    assert_eq!(out.rows.len(), 2);
    assert_eq!(
        out.rows[0].title.as_deref(),
        Some("A Deep Reinforced Model for Abstractive Summarization")
    );
    assert_eq!(out.rows[0].ids["arxiv"], "1705.04304");
    assert_eq!(
        out.rows[0].volume, None,
        "arXiv's abs/… volume is not a volume"
    );
    assert_eq!(out.next.as_deref(), Some("2"));
    let out = s2
        .related(
            "arxiv:1706.03762",
            Relation::CitedBy,
            PageRequest {
                limit: 2,
                cursor: Some("4".into()),
                sort: None,
            },
        )
        .await
        .unwrap();
    cites.assert_async().await;
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.rows[0].ids["doi"], "10.1016/j.radphyschem.2026.114346");
    assert_eq!(out.next.as_deref(), Some("2"));
    let d = s2
        .related("arxiv:1706.03762", Relation::Coreads, page(2))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
}

#[tokio::test]
async fn similar_resolves_a_paper_id_first_and_asks_the_all_cs_pool() {
    let (mut server, s2) = adapter().await;
    let resolve = server
        .mock("GET", "/graph/v1/paper/ARXIV:1706.03762")
        .match_query(Matcher::UrlEncoded("fields".into(), "paperId".into()))
        .with_status(200)
        .with_body(json!({"paperId": TRANSFORMER}).to_string())
        .expect(1)
        .create_async()
        .await;
    let rec = server
        .mock(
            "GET",
            &format!("/recommendations/v1/papers/forpaper/{TRANSFORMER}")[..],
        )
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("from".into(), "all-cs".into()),
            Matcher::UrlEncoded("limit".into(), "2".into()),
        ]))
        .with_status(200)
        .with_body(fixture("recommendations.json"))
        .create_async()
        .await;
    let out = s2
        .related("arxiv:1706.03762", Relation::Similar, page(2))
        .await
        .unwrap();
    resolve.assert_async().await;
    rec.assert_async().await;
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.rows[0].title.as_deref(), Some("Transformer++"));
    assert_eq!(out.next, None, "the recommendations API does not page");
    // A paper id needs no resolving, and a query with `similar_to` is the same walk.
    let rec2 = server
        .mock(
            "GET",
            &format!("/recommendations/v1/papers/forpaper/{TRANSFORMER}")[..],
        )
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("recommendations.json"))
        .create_async()
        .await;
    let out = s2
        .search(
            &Query::clause("similar_to", "=", format!("s2:{TRANSFORMER}")),
            page(2),
        )
        .await
        .unwrap();
    rec2.assert_async().await;
    assert_eq!(out.rows.len(), 2);
}

#[tokio::test]
async fn the_api_key_goes_out_as_x_api_key_only_when_the_secret_is_set() {
    let (mut server, s2) = adapter().await;
    let keyless = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .match_header("x-api-key", Matcher::Missing)
        .with_status(200)
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    s2.search(&Query::clause("any", "any", "attention"), page(3))
        .await
        .unwrap();
    keyless.assert_async().await;
    keyless.remove_async().await;

    let keyed = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .match_header("x-api-key", "test-key-not-real")
        .with_status(200)
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    std::env::set_var("KOINE_SECRET_S2_API_KEY", "test-key-not-real");
    let s2b = SemanticScholar::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()));
    s2b.limiter()
        .set_limit(SOURCE, 1000, Duration::from_secs(1));
    let result = s2b
        .search(&Query::clause("any", "any", "attention"), page(3))
        .await;
    std::env::remove_var("KOINE_SECRET_S2_API_KEY");
    result.unwrap();
    keyed.assert_async().await;
}

#[tokio::test]
async fn the_recorded_429_backs_off_and_the_next_call_sends_nothing() {
    let (mut server, s2) = adapter().await;
    let m = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_body(fixture("rate-limited-429.json"))
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "attention");
    let d = s2.search(&q, page(3)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("429"));
    assert!(s2.limiter().backoff_remaining(SOURCE) > Duration::ZERO);
    let d = s2.search(&q, page(3)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("nothing was sent"));
    m.assert_async().await;
}

#[tokio::test]
async fn export_is_csl_only() {
    let (mut server, s2) = adapter().await;
    let _m = server
        .mock("GET", "/graph/v1/paper/ARXIV:1706.03762")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-arxiv.json"))
        .create_async()
        .await;
    let text = s2
        .export(&["arxiv:1706.03762".into()], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v[0]["type"], json!("paper-conference"));
    assert_eq!(v[0]["custom"]["ids"]["s2"], json!(TRANSFORMER));
    for f in [ExportFormat::Bibtex, ExportFormat::Ris] {
        let d = s2
            .export(&["arxiv:1706.03762".into()], f)
            .await
            .unwrap_err();
        assert_eq!(d.status, Status::Unsupported);
    }
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end() {
    let (mut server, s2) = adapter().await;
    let _s = server
        .mock("GET", "/graph/v1/paper/search")
        .match_query(Matcher::UrlEncoded(
            "query".into(),
            "attention is all you need".into(),
        ))
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let _f = server
        .mock("GET", "/graph/v1/paper/CorpusId:13756489")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-corpusid.json"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(s2));
    let service = DefaultBibSourceService::with_registry(registry);
    let explain = service.explain(None).await.answer.unwrap();
    assert_eq!(explain.sources[0].source, "s2");
    let answer = service
        .search(
            Query::clause("title", "any", "attention is all you need"),
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
    assert!(answer.next.unwrap().contains("s2"));
    // `CorpusId:13756489` as S2 writes it.
    let r = service.fetch("CorpusId:13756489".into(), None, None).await;
    assert_eq!(r.answer.unwrap().work.ids["s2"], TRANSFORMER);
}

/// Live, opt-in: `cargo test -p source-s2 -- --ignored live`. Needs the
/// network; without `KOINE_SECRET_S2_API_KEY` the shared pool may answer 429,
/// which this test reports rather than hides.
#[tokio::test]
#[ignore = "reaches api.semanticscholar.org"]
async fn live_search_and_fetch() {
    let s2 = SemanticScholar::new().with_limiter(Arc::new(Limiter::new()));
    let out = s2
        .search(
            &Query::clause("title", "any", "attention is all you need"),
            page(3),
        )
        .await
        .unwrap();
    assert!(!out.rows.is_empty());
    let work = s2
        .fetch("arxiv:1706.03762")
        .await
        .unwrap()
        .expect("the Transformer paper exists");
    assert_eq!(work.ids["s2"], TRANSFORMER);
    assert_eq!(work.title.as_deref(), Some("Attention is All you Need"));
}
