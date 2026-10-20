//! Tier A over the recorded fixtures (`fixtures/README.md`): the HTTP layer
//! is a mock server that serves them; nothing here reaches inspirehep.net
//! except the one `#[ignore]` live test at the end.

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

use crate::{Inspire, FIELDS, SOURCE};

const DOI: &str = "10.1126/science.1063991";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

async fn adapter() -> (ServerGuard, Inspire) {
    let server = Server::new_async().await;
    let inspire = Inspire::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    (server, inspire)
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort: None,
    }
}

#[test]
fn the_explain_record_is_valid_and_declares_its_native_dialect() {
    let record = crate::explain_json();
    assert_eq!(record.source, "inspire");
    assert_eq!(record.native.as_deref(), Some("inspire"));
    for index in [
        "any",
        "title",
        "author",
        "first_author",
        "year",
        "doi",
        "arxiv",
        "container",
        "keyword",
        "cites",
        "cited_by",
    ] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    assert_eq!(record.page.max, Some(100));
    assert_eq!(record.etiquette.limit.as_deref(), Some("15/5s"));
    assert!(
        record.etiquette.budget.is_none(),
        "no daily figure is documented"
    );
    assert!(record.secrets.is_empty());
    let a = Inspire::new();
    assert!(a.detect("doi:10.1126/science.1063991"));
    assert!(a.detect("arxiv:astro-ph/0112088"));
    assert!(a.detect("inspire:728142"));
    assert!(a.detect("recid:728142"));
    assert!(!a.detect("inspire:abc"));
    assert!(!a.detect("728142"), "a bare number is not claimed");
    assert!(!a.detect("pmid:123"));
    assert!(!a.detect("bibcode:2002Sci...295...93A"));
}

#[tokio::test]
async fn a_search_sends_the_spires_query_page_size_sort_and_fields() {
    let (mut server, inspire) = adapter().await;
    let m = server
        .mock("GET", "/literature")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "a Abel and (t first and t stars)".into()),
            Matcher::UrlEncoded("size".into(), "3".into()),
            Matcher::UrlEncoded("page".into(), "1".into()),
            Matcher::UrlEncoded("sort".into(), "mostcited".into()),
            Matcher::UrlEncoded("fields".into(), FIELDS.into()),
        ]))
        .match_header(
            "user-agent",
            Matcher::Regex("koine-source-inspire/.*mailto:tabel@stanford.edu".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel"),
            Query::clause("title", "any", "first stars"),
        ],
    };
    let out = inspire
        .search(
            &q,
            PageRequest {
                limit: 3,
                cursor: None,
                sort: Some("cited_by".into()),
            },
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert_eq!(out.rows.len(), 3);
    assert!(out.diagnostics.is_empty());
    assert_eq!(out.next.as_deref(), Some("2"), "7 hits, 3 seen");
    let row = &out.rows[0];
    assert_eq!(row.provenance.as_ref().unwrap().source_id, "764484");
    assert_eq!(row.ids["inspire"], "764484", "the recid is an id kind");
    assert_eq!(
        row.title.as_deref(),
        Some("Resolving the Formation of Protogalaxies. 3. Feedback from the First Stars")
    );
    assert_eq!(
        row.metrics
            .as_ref()
            .unwrap()
            .cited_by
            .as_ref()
            .unwrap()
            .value,
        200
    );
    let prov = row.provenance.as_ref().unwrap();
    assert_eq!(
        (prov.source.as_str(), prov.source_id.as_str()),
        (SOURCE, "764484")
    );
    assert!(prov.query.as_deref().unwrap().contains("first stars"));
    for r in &out.rows {
        r.validate().unwrap();
    }
    // The documented burst limit was declared to the limiter.
    assert_eq!(
        inspire.limiter().limit_of(SOURCE),
        Some((15, Duration::from_secs(5)))
    );
}

#[tokio::test]
async fn year_and_first_author_and_the_last_page_has_no_next() {
    let (mut server, inspire) = adapter().await;
    let m = server
        .mock("GET", "/literature")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "fa Abel and date 2002".into()),
            Matcher::UrlEncoded("size".into(), "2".into()),
        ]))
        .with_status(200)
        .with_body(fixture("year.json"))
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("first_author", "any", "Abel"),
            Query::clause("year", "=", "2002"),
        ],
    };
    let out = inspire.search(&q, page(2)).await.unwrap();
    m.assert_async().await;
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.rows[0].provenance.as_ref().unwrap().source_id, "587766");
    assert_eq!(out.next.as_deref(), Some("2"), "8 hits, 2 seen");
    // The same page asked with room to spare is the end.
    let (mut server, inspire) = adapter().await;
    let _m = server
        .mock("GET", "/literature")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("year.json"))
        .create_async()
        .await;
    let out = inspire.search(&q, page(100)).await.unwrap();
    assert_eq!(out.next, None, "two rows of a hundred asked");
}

#[tokio::test]
async fn a_native_query_passes_through_and_unsupported_clauses_are_diagnostics() {
    let (mut server, inspire) = adapter().await;
    let m = server
        .mock("GET", "/literature")
        .match_query(Matcher::UrlEncoded(
            "q".into(),
            "t \"first star\" and date 2002".into(),
        ))
        .with_status(200)
        .with_body(fixture("year.json"))
        .create_async()
        .await;
    let out = inspire
        .search(
            &Query::Native {
                native: "t \"first star\" and date 2002".into(),
            },
            page(10),
        )
        .await
        .unwrap();
    m.assert_async().await;
    assert_eq!(out.rows.len(), 2);

    // A wholly unsupported query sends nothing.
    let nothing = server
        .mock("GET", "/literature")
        .expect(0)
        .create_async()
        .await;
    let out = inspire
        .search(&Query::clause("pmid", "=", "11729294"), page(5))
        .await
        .unwrap();
    nothing.assert_async().await;
    assert!(out.rows.is_empty());
    assert_eq!(out.diagnostics[0].status, Status::Unsupported);
    assert_eq!(out.diagnostics[0].detail, "bib.pmid =");
}

#[tokio::test]
async fn a_cursor_past_the_ten_thousand_hit_window_sends_nothing() {
    let (mut server, inspire) = adapter().await;
    let nothing = server
        .mock("GET", "/literature")
        .expect(0)
        .create_async()
        .await;
    let out = inspire
        .search(
            &Query::clause("any", "any", "dark matter"),
            PageRequest {
                limit: 100,
                cursor: Some("101".into()),
                sort: None,
            },
        )
        .await
        .unwrap();
    nothing.assert_async().await;
    assert!(out.rows.is_empty() && out.next.is_none());
    assert_eq!(out.diagnostics[0].status, Status::Unsupported);
    assert!(out.diagnostics[0].detail.contains("10000"));
}

#[tokio::test]
async fn fetch_by_recid_has_every_id_both_locations_and_inspires_conventions() {
    let (mut server, inspire) = adapter().await;
    let m = server
        .mock("GET", "/literature/728142")
        .match_query(Matcher::UrlEncoded("fields".into(), FIELDS.into()))
        .with_status(200)
        .with_body(fixture("fetch-recid.json"))
        .create_async()
        .await;
    let work = inspire.fetch("inspire:728142").await.unwrap().unwrap();
    m.assert_async().await;
    work.validate().unwrap();
    assert_eq!(work.ty, "article-journal");
    assert_eq!(
        work.title.as_deref(),
        Some("The first generation of stars in lambda-CDM cosmology")
    );
    assert_eq!(work.provenance.as_ref().unwrap().source_id, "728142");
    assert_eq!(work.ids["doi"], "10.1111/j.1365-2966.2007.11814.x");
    assert_eq!(work.ids["arxiv"], "astro-ph/0610174");
    assert_eq!(work.ids["bibcode"], "2007MNRAS.378..449G");
    assert_eq!(
        work.ids["inspire"],
        work.provenance.as_ref().unwrap().source_id,
        "the recid is an id kind"
    );
    assert_eq!(work.author.len(), 6);
    assert_eq!(work.author[0].family.as_deref(), Some("Gao"));
    assert_eq!(work.author[0].given.as_deref(), Some("Liang"));
    assert_eq!(work.issued.as_ref().unwrap().date_parts, vec![vec![2007]]);
    assert_eq!(
        work.container_title.as_deref(),
        Some("Mon.Not.Roy.Astron.Soc.")
    );
    assert_eq!(work.volume.as_deref(), Some("378"));
    assert_eq!(work.page.as_deref(), Some("449"));
    assert_eq!(
        work.url.as_deref(),
        Some("https://inspirehep.net/literature/728142")
    );
    assert!(work
        .abstract_
        .as_deref()
        .unwrap()
        .starts_with("We have performed"));
    let published = &work.locations[0];
    assert_eq!(published.version, Version::Published);
    assert!(published.primary);
    assert_eq!(
        published.url.as_deref(),
        Some("https://doi.org/10.1111/j.1365-2966.2007.11814.x")
    );
    let arxiv = &work.locations[1];
    assert_eq!(arxiv.version, Version::Submitted);
    assert!(!arxiv.primary);
    assert_eq!(
        arxiv.url.as_deref(),
        Some("https://arxiv.org/abs/astro-ph/0610174")
    );
    assert_eq!(
        arxiv.pdf.as_deref(),
        Some("https://arxiv.org/pdf/astro-ph/0610174")
    );
    let hosted = &work.locations[2];
    assert_eq!(hosted.version, Version::Submitted);
    assert!(hosted
        .pdf
        .as_deref()
        .unwrap()
        .starts_with("https://inspirehep.net/files/"));
    assert_eq!(work.locations.iter().filter(|l| l.primary).count(), 1);
    assert_eq!(work.metrics.unwrap().cited_by.unwrap().value, 99);
}

#[tokio::test]
async fn fetch_by_doi_and_by_arxiv_id_use_their_own_endpoints_and_a_404_is_none() {
    let (mut server, inspire) = adapter().await;
    let d = server
        .mock("GET", "/doi/10.1126/science.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-doi.json"))
        .create_async()
        .await;
    let a = server
        .mock("GET", "/arxiv/astro-ph/0112088")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-arxiv.json"))
        .create_async()
        .await;
    let missing = server
        .mock("GET", "/literature/999999999")
        .match_query(Matcher::Any)
        .with_status(404)
        .with_body(fixture("not-found.json"))
        .create_async()
        .await;
    let by_doi = inspire.fetch(&format!("doi:{DOI}")).await.unwrap().unwrap();
    let by_arxiv = inspire
        .fetch("arxiv:astro-ph/0112088")
        .await
        .unwrap()
        .unwrap();
    d.assert_async().await;
    a.assert_async().await;
    assert_eq!(by_doi.provenance.as_ref().unwrap().source_id, "578281");
    assert_eq!(by_doi, {
        let mut w = by_arxiv.clone();
        w.provenance = by_doi.provenance.clone();
        w
    });
    assert_eq!(
        by_doi.doi.as_deref(),
        Some(DOI),
        "the publisher DOI is the first one"
    );
    assert_eq!(by_doi.author.len(), 3);
    assert_eq!(by_doi.author[1].family.as_deref(), Some("Bryan"));
    assert_eq!(by_doi.author[1].given.as_deref(), Some("Greg L."));
    assert_eq!(
        by_doi
            .metrics
            .as_ref()
            .unwrap()
            .cited_by
            .as_ref()
            .unwrap()
            .value,
        1174
    );
    assert_eq!(by_doi.container_title.as_deref(), Some("Science"));
    assert_eq!(inspire.fetch("inspire:999999999").await.unwrap(), None);
    missing.assert_async().await;
    // Not an id this adapter claims: nothing is asked.
    assert_eq!(inspire.fetch("pmid:11729294").await.unwrap(), None);
}

#[tokio::test]
async fn a_collaboration_paper_shows_the_collaboration_not_its_author_list() {
    let (mut server, inspire) = adapter().await;
    let _m = server
        .mock("GET", "/literature")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("collaboration.json"))
        .create_async()
        .await;
    let out = inspire
        .search(&Query::clause("any", "any", "CMS boson"), page(1))
        .await
        .unwrap();
    let work = &out.rows[0];
    assert_eq!(work.author.len(), 1);
    assert_eq!(work.author[0].family.as_deref(), Some("CMS Collaboration"));
}

#[tokio::test]
async fn a_429_backs_off_and_the_next_call_sends_nothing() {
    let (mut server, inspire) = adapter().await;
    let m = server
        .mock("GET", "/literature")
        .match_query(Matcher::Any)
        .with_status(429)
        .with_header("retry-after", "30")
        .with_body("{\"message\": \"Too many requests\", \"status\": 429}")
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "dark matter");
    let d = inspire.search(&q, page(5)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("30s"));
    let d = inspire.search(&q, page(5)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("nothing was sent"));
    m.assert_async().await;
}

#[tokio::test]
async fn a_5xx_is_an_error_diagnostic() {
    let (mut server, inspire) = adapter().await;
    let _m = server
        .mock("GET", "/literature")
        .match_query(Matcher::Any)
        .with_status(502)
        .create_async()
        .await;
    let d = inspire
        .search(&Query::clause("any", "any", "x"), page(5))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
}

#[tokio::test]
async fn export_bibtex_is_inspires_own_and_csl_is_built_from_the_record() {
    let (mut server, inspire) = adapter().await;
    let bib = server
        .mock("GET", "/literature/728142")
        .match_query(Matcher::UrlEncoded("format".into(), "bibtex".into()))
        .match_header("accept", "application/x-bibtex")
        .with_status(200)
        .with_header("content-type", "application/x-bibtex")
        .with_body(fixture("export.bib"))
        .create_async()
        .await;
    let text = inspire
        .export(&["inspire:728142".into()], ExportFormat::Bibtex)
        .await
        .unwrap();
    bib.assert_async().await;
    assert!(text.starts_with("@article{Gao:2006ug,"));
    assert!(text.contains("archivePrefix = \"arXiv\""));

    let _rec = server
        .mock("GET", "/literature/728142")
        .match_query(Matcher::UrlEncoded("fields".into(), FIELDS.into()))
        .with_status(200)
        .with_body(fixture("fetch-recid.json"))
        .create_async()
        .await;
    let csl = inspire
        .export(&["inspire:728142".into()], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&csl).unwrap();
    assert_eq!(v[0]["type"], json!("article-journal"));
    assert_eq!(v[0]["custom"]["ids"]["arxiv"], json!("astro-ph/0610174"));

    let ris = inspire
        .export(&["inspire:728142".into()], ExportFormat::Ris)
        .await
        .unwrap_err();
    assert_eq!(ris.status, Status::Unsupported);
}

#[tokio::test]
async fn related_walks_refersto_and_citedby_by_recid_resolving_a_doi_first() {
    let (mut server, inspire) = adapter().await;
    let cited_by = server
        .mock("GET", "/literature")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "refersto:recid:728142".into()),
            Matcher::UrlEncoded("size".into(), "2".into()),
        ]))
        .with_status(200)
        .with_body(fixture("cited-by.json"))
        .create_async()
        .await;
    let page_out = inspire
        .related("inspire:728142", Relation::CitedBy, page(2))
        .await
        .unwrap();
    cited_by.assert_async().await;
    assert_eq!(page_out.rows.len(), 2);
    assert_eq!(page_out.next.as_deref(), Some("2"), "99 citing works");

    let refs = server
        .mock("GET", "/literature")
        .match_query(Matcher::UrlEncoded(
            "q".into(),
            "citedby:recid:578281".into(),
        ))
        .with_status(200)
        .with_body(fixture("references.json"))
        .create_async()
        .await;
    let resolve = server
        .mock("GET", "/doi/10.1126/science.1063991")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_body(fixture("fetch-doi.json"))
        .expect(1)
        .create_async()
        .await;
    let page_out = inspire
        .related(&format!("doi:{DOI}"), Relation::Cites, page(2))
        .await
        .unwrap();
    resolve.assert_async().await;
    refs.assert_async().await;
    assert_eq!(page_out.rows.len(), 2);
    assert_eq!(
        page_out.rows[0].provenance.as_ref().unwrap().source_id,
        "613517"
    );

    let d = inspire
        .related("inspire:728142", Relation::Similar, page(2))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Unsupported);
    assert_eq!(d.detail, "similar");
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end_including_inspire_ids() {
    let (mut server, inspire) = adapter().await;
    let _s = server
        .mock("GET", "/literature")
        .match_query(Matcher::UrlEncoded(
            "q".into(),
            "a Abel and (t first and t stars)".into(),
        ))
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let _f = server
        .mock("GET", "/literature/728142")
        .match_query(Matcher::UrlEncoded("fields".into(), FIELDS.into()))
        .with_status(200)
        .with_body(fixture("fetch-recid.json"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(inspire));
    let service = DefaultBibSourceService::with_registry(registry);

    let explain = service.explain(None).await.answer.unwrap();
    assert_eq!(explain.sources[0].source, "inspire");

    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Abel"),
            Query::clause("title", "any", "first stars"),
        ],
    };
    let answer = service
        .search(q, None, Some(3), None, None, None)
        .await
        .answer
        .expect("ok");
    assert_eq!(answer.rows.len(), 3);
    assert!(answer.next.unwrap().contains("inspire"));

    // `recid:` is pasted, `inspire:` is canonical.
    let r = service.fetch("recid:728142".into(), None, None).await;
    assert_eq!(
        r.answer.unwrap().work.provenance.unwrap().source_id,
        "728142"
    );
}

/// Live, opt-in: `cargo test -p source-inspire -- --ignored live`. Needs the
/// network; no key.
#[tokio::test]
#[ignore = "reaches inspirehep.net"]
async fn live_search_and_fetch() {
    let inspire = Inspire::new().with_limiter(Arc::new(Limiter::new()));
    let out = inspire
        .search(
            &Query::And {
                and: vec![
                    Query::clause("first_author", "any", "Abel"),
                    Query::clause("year", "=", "2002"),
                ],
            },
            page(3),
        )
        .await
        .unwrap();
    assert!(!out.rows.is_empty());
    assert!(out.rows.iter().all(|r| r
        .url
        .as_deref()
        .unwrap()
        .contains("inspirehep.net/literature/")));
    let work = inspire
        .fetch(&format!("doi:{DOI}"))
        .await
        .unwrap()
        .expect("Abel 2002 exists");
    assert_eq!(work.container_title.as_deref(), Some("Science"));
    assert_eq!(work.ids["arxiv"], "astro-ph/0112088");
    let bib = inspire
        .export(&[format!("doi:{DOI}")], ExportFormat::Bibtex)
        .await
        .unwrap();
    assert!(bib.contains("Abel:2001pr"));
}
