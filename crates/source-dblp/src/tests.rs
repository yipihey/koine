//! Tier A over the fixtures (`fixtures/README.md`): the HTTP layer is a mock
//! server that serves them; nothing here reaches dblp.org except the one
//! `#[ignore]` live test at the end. The search and record fixtures are
//! hand-written from DBLP's documented shapes (dblp.org answered every real
//! request with a bot-check page); `bot-challenge.*` is the recorded proof.

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

use crate::{Dblp, SOURCE};

const KEY: &str = "conf/nips/VaswaniSPUJGKP17";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

async fn adapter() -> (ServerGuard, Dblp) {
    let server = Server::new_async().await;
    let dblp = Dblp::new()
        .with_base_url(server.url())
        .with_limiter(Arc::new(Limiter::new()))
        .with_contact(Some("mailto:tabel@stanford.edu".into()));
    dblp.limiter()
        .set_limit(SOURCE, 1000, Duration::from_secs(1));
    (server, dblp)
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort: None,
    }
}

#[test]
fn the_explain_record_is_valid_and_detect_claims_only_dblp_keys() {
    let record = crate::explain_json();
    assert_eq!(record.source, "dblp");
    assert_eq!(record.page.max, Some(100));
    assert_eq!(record.etiquette.limit.as_deref(), Some("1/s sustained"));
    assert!(record.native.is_none());
    assert!(!record.operations.contains(&"related".to_string()));
    for index in ["any", "title", "author", "year", "container"] {
        assert!(record.indexes.contains_key(index), "{index}");
    }
    // `dblp` is not in the bib index set: a key is reached through fetch.
    assert!(!record.indexes.contains_key("dblp"));
    assert_eq!(record.identifiers, ["dblp"]);
    let a = Dblp::new();
    for yes in [
        "dblp:conf/nips/VaswaniSPUJGKP17",
        "dblp:journals/jmlr/BleiNJ03",
        "dblp:journals/corr/abs-1706-03762",
        "dblp:books/sp/17/Smith17",
        "dblp:phd/dnb/Smith01",
        "dblp:series/lncs/Smith05",
        "dblp:conf/nips/VaswaniSPUJGKP17.html",
    ] {
        assert!(a.detect(yes), "{yes}");
    }
    for no in [
        "doi:10.1126/science.1063991",
        "arxiv:astro-ph/0112088",
        "dblp:nips/Vaswani",
        "dblp:conf/nips",
        "dblp:www/x/y",
        "conf/nips/VaswaniSPUJGKP17",
    ] {
        assert!(!a.detect(no), "{no}");
    }
}

#[tokio::test]
async fn a_search_sends_q_format_h_f_and_parses_object_array_and_single_shapes() {
    let (mut server, dblp) = adapter().await;
    let m = server
        .mock("GET", "/search/publ/api")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "Vaswani attention is all".into()),
            Matcher::UrlEncoded("format".into(), "json".into()),
            Matcher::UrlEncoded("h".into(), "3".into()),
            Matcher::UrlEncoded("f".into(), "0".into()),
        ]))
        .match_header(
            "user-agent",
            Matcher::Regex("koine-source-dblp/.*mailto:tabel@stanford.edu".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(fixture("search.json"))
        .expect(1)
        .create_async()
        .await;
    let q = Query::And {
        and: vec![
            Query::clause("author", "any", "Vaswani"),
            Query::clause("title", "any", "attention is all"),
        ],
    };
    let out = dblp.search(&q, page(3)).await.unwrap();
    m.assert_async().await;
    assert_eq!(out.rows.len(), 3);
    assert_eq!(out.next.as_deref(), Some("3"), "142 hits, 3 sent");
    // The restriction DBLP cannot make is reported, once per clause.
    assert_eq!(out.diagnostics.len(), 2);
    assert!(out
        .diagnostics
        .iter()
        .all(|d| d.status == Status::Unsupported && d.detail.contains("cannot restrict")));

    let t = &out.rows[0];
    t.validate().unwrap();
    assert_eq!(
        t.title.as_deref(),
        Some("Attention is All you Need"),
        "no trailing stop"
    );
    assert_eq!(t.ty, "paper-conference");
    assert_eq!(t.ids["dblp"], KEY);
    assert_eq!(t.container_title.as_deref(), Some("NIPS"));
    assert_eq!(t.page.as_deref(), Some("5998-6008"));
    assert_eq!(t.issued.as_ref().unwrap().date_parts, vec![vec![2017]]);
    assert_eq!(t.author.len(), 8);
    assert_eq!(t.author[5].family.as_deref(), Some("Gomez"));
    assert_eq!(t.author[5].given.as_deref(), Some("Aidan N."));
    assert_eq!(t.locations.len(), 1);
    assert_eq!(t.locations[0].version, Version::Published);
    assert!(t.locations[0].primary);
    assert_eq!(
        t.url.as_deref(),
        Some("https://dblp.org/rec/conf/nips/VaswaniSPUJGKP17")
    );
    assert_eq!(t.provenance.as_ref().unwrap().source_id, KEY);

    // A single author is an object, not a list; the homonym number goes; a
    // `doi` field is an id; `volume` is kept.
    let j = &out.rows[1];
    j.validate().unwrap();
    assert_eq!(j.ty, "article-journal");
    assert_eq!(j.author.len(), 1);
    assert_eq!(j.author[0].family.as_deref(), Some("Blei"));
    assert_eq!(j.author[0].given.as_deref(), Some("David M."));
    assert_eq!(j.ids["doi"], "10.5555/944919.944937");
    assert_eq!(j.volume.as_deref(), Some("3"));

    // CoRR: an array venue, `abs/…` volume dropped, arXiv + DOI from `ee`,
    // every location `submitted`.
    let c = &out.rows[2];
    c.validate().unwrap();
    assert_eq!(c.ty, "article");
    assert_eq!(c.container_title.as_deref(), Some("CoRR"));
    assert_eq!(c.volume, None);
    assert_eq!(c.ids["arxiv"], "1706.03762");
    assert_eq!(c.ids["doi"], "10.48550/arXiv.1706.03762");
    assert_eq!(c.locations.len(), 2);
    assert!(c.locations.iter().all(|l| l.version == Version::Submitted));
    assert_eq!(c.locations.iter().filter(|l| l.primary).count(), 1);
}

#[tokio::test]
async fn no_hits_is_an_empty_page_and_a_cursor_is_the_next_offset() {
    let (mut server, dblp) = adapter().await;
    let empty = server
        .mock("GET", "/search/publ/api")
        .match_query(Matcher::UrlEncoded("q".into(), "zzzqqqxxnomatch".into()))
        .with_status(200)
        .with_body(fixture("search-empty.json"))
        .create_async()
        .await;
    let out = dblp
        .search(&Query::clause("any", "any", "zzzqqqxxnomatch"), page(10))
        .await
        .unwrap();
    empty.assert_async().await;
    assert!(out.rows.is_empty() && out.next.is_none() && out.diagnostics.is_empty());

    let second = server
        .mock("GET", "/search/publ/api")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "attention".into()),
            Matcher::UrlEncoded("f".into(), "30".into()),
            Matcher::UrlEncoded("h".into(), "3".into()),
        ]))
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let out = dblp
        .search(
            &Query::clause("any", "any", "attention"),
            PageRequest {
                limit: 3,
                cursor: Some("30".into()),
                sort: None,
            },
        )
        .await
        .unwrap();
    second.assert_async().await;
    // The fixture says @first 0, @sent 3 of 142; the offset we asked is ours.
    assert_eq!(out.next.as_deref(), Some("33"));
}

#[tokio::test]
async fn unsupported_queries_send_nothing() {
    let (mut server, dblp) = adapter().await;
    let nothing = server
        .mock("GET", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let out = dblp
        .search(&Query::clause("year", ">", "2000"), page(5))
        .await
        .unwrap();
    assert!(out.rows.is_empty());
    assert_eq!(out.diagnostics[0].status, Status::Unsupported);
    assert!(out.diagnostics[0].detail.starts_with("bib.year >"));
    nothing.assert_async().await;
}

#[tokio::test]
async fn fetch_by_key_reads_the_record_xml_and_a_404_is_none() {
    let (mut server, dblp) = adapter().await;
    let m = server
        .mock("GET", "/rec/conf/nips/VaswaniSPUJGKP17.xml")
        .with_status(200)
        .with_header("content-type", "application/xml")
        .with_body(fixture("rec.xml"))
        .create_async()
        .await;
    let w = dblp.fetch(&format!("dblp:{KEY}")).await.unwrap().unwrap();
    m.assert_async().await;
    w.validate().unwrap();
    assert_eq!(w.ids["dblp"], KEY);
    assert_eq!(w.title.as_deref(), Some("Attention is All you Need"));
    assert_eq!(w.ty, "paper-conference");
    assert_eq!(w.container_title.as_deref(), Some("NIPS"));
    assert_eq!(w.author.len(), 8);
    assert_eq!(w.author[0].family.as_deref(), Some("Vaswani"));
    assert_eq!(w.page.as_deref(), Some("5998-6008"));
    assert_eq!(w.locations[0].version, Version::Published);
    assert!(w.locations[0]
        .url
        .as_deref()
        .unwrap()
        .contains("proceedings.neurips.cc"));

    let a = server
        .mock("GET", "/rec/journals/jmlr/BleiNJ03.xml")
        .with_status(200)
        .with_header("content-type", "text/xml")
        .with_body(fixture("rec-article.xml"))
        .create_async()
        .await;
    let w = dblp
        .fetch("dblp:journals/jmlr/BleiNJ03")
        .await
        .unwrap()
        .unwrap();
    a.assert_async().await;
    assert_eq!(w.ty, "article-journal");
    assert_eq!(w.volume.as_deref(), Some("3"));
    assert_eq!(w.container_title.as_deref(), Some("J. Mach. Learn. Res."));
    assert_eq!(
        w.doi.as_deref(),
        Some("10.5555/944919.944937"),
        "from the doi.org ee"
    );
    assert_eq!(
        w.author[0].given.as_deref(),
        Some("David M."),
        "homonym number dropped"
    );
    assert_eq!(w.locations.len(), 2);

    let _nf = server
        .mock("GET", "/rec/conf/nips/Nobody99.xml")
        .with_status(404)
        .with_header("content-type", "text/html")
        .with_body("Not Found")
        .create_async()
        .await;
    // A 404 page is HTML; only 202 or an HTML 200 is the bot check.
    assert_eq!(dblp.fetch("dblp:conf/nips/Nobody99").await.unwrap(), None);
    assert_eq!(dblp.fetch("doi:10.1/x").await.unwrap(), None);
}

#[test]
fn xml_entities_inline_markup_and_editors() {
    let xml = r#"<?xml version="1.0" encoding="US-ASCII"?>
<dblp><proceedings key="conf/x/Y20">
<editor>Jos&#233; M&#252;ller</editor><editor>Ana Ruiz</editor>
<title>Proc. of the <i>X</i> &amp; Y Conference.</title><year>2020</year>
<booktitle>XY</booktitle></proceedings></dblp>"#;
    let r = crate::parse::record_from_xml(xml).unwrap();
    assert_eq!(r.title.as_deref(), Some("Proc. of the X & Y Conference."));
    assert_eq!(r.authors, ["José Müller", "Ana Ruiz"]);
    assert_eq!(r.ty, "book");
    assert!(crate::parse::record_from_xml("<dblp></dblp>").is_none());
}

#[tokio::test]
async fn export_bibtex_is_dblps_own_and_csl_is_built_from_the_record() {
    let (mut server, dblp) = adapter().await;
    let bib = server
        .mock("GET", "/rec/conf/nips/VaswaniSPUJGKP17.bib")
        .with_status(200)
        .with_header("content-type", "text/plain; charset=utf-8")
        .with_body(fixture("rec.bib"))
        .create_async()
        .await;
    let text = dblp
        .export(&[format!("dblp:{KEY}")], ExportFormat::Bibtex)
        .await
        .unwrap();
    bib.assert_async().await;
    assert!(text.starts_with("@inproceedings{DBLP:conf/nips/VaswaniSPUJGKP17,"));
    assert!(text.contains("bibsource    = {dblp computer science bibliography, https://dblp.org}"));

    let _rec = server
        .mock("GET", "/rec/conf/nips/VaswaniSPUJGKP17.xml")
        .with_status(200)
        .with_body(fixture("rec.xml"))
        .create_async()
        .await;
    let csl = dblp
        .export(&[format!("dblp:{KEY}")], ExportFormat::CslJson)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&csl).unwrap();
    assert_eq!(v[0]["type"], json!("paper-conference"));
    assert_eq!(v[0]["custom"]["ids"]["dblp"], json!(KEY));
    let ris = dblp
        .export(&[format!("dblp:{KEY}")], ExportFormat::Ris)
        .await
        .unwrap_err();
    assert_eq!(ris.status, Status::Unsupported);
}

#[tokio::test]
async fn related_is_unsupported() {
    let dblp = Dblp::new().with_limiter(Arc::new(Limiter::new()));
    for r in [Relation::Cites, Relation::CitedBy, Relation::Similar] {
        let d = dblp
            .related(&format!("dblp:{KEY}"), r, page(5))
            .await
            .unwrap_err();
        assert_eq!(d.status, Status::Unsupported);
        assert_eq!(d.detail, r.as_str());
    }
}

#[tokio::test]
async fn the_recorded_bot_check_is_an_error_that_says_so_not_a_parse_failure() {
    let (mut server, dblp) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .with_status(202)
        .with_header("content-type", "text/html; charset=utf-8")
        .with_body(fixture("bot-challenge.html"))
        .expect(3)
        .create_async()
        .await;
    let d = dblp
        .search(&Query::clause("any", "any", "attention"), page(3))
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    assert!(d.detail.contains("bot-check"), "{}", d.detail);
    assert!(d.detail.contains("does not solve"));
    let d = dblp.fetch(&format!("dblp:{KEY}")).await.unwrap_err();
    assert!(d.detail.contains("bot-check"));
    let d = dblp
        .export(&[format!("dblp:{KEY}")], ExportFormat::Bibtex)
        .await
        .unwrap_err();
    assert_eq!(d.status, Status::Error);
    m.assert_async().await;
    // And the same page served as a 200 is still not data.
    let (mut server, dblp) = adapter().await;
    let _ = server
        .mock("GET", Matcher::Any)
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(fixture("bot-challenge.html"))
        .create_async()
        .await;
    assert!(dblp.fetch(&format!("dblp:{KEY}")).await.is_err());
}

#[tokio::test]
async fn a_429_backs_off_and_the_next_call_sends_nothing() {
    let (mut server, dblp) = adapter().await;
    let m = server
        .mock("GET", Matcher::Any)
        .with_status(429)
        .with_header("retry-after", "20")
        .expect(1)
        .create_async()
        .await;
    let q = Query::clause("any", "any", "attention");
    let d = dblp.search(&q, page(3)).await.unwrap_err();
    assert_eq!(d.status, Status::RateLimited);
    assert!(d.detail.contains("20s"));
    let d = dblp.search(&q, page(3)).await.unwrap_err();
    assert!(d.detail.contains("nothing was sent"));
    m.assert_async().await;
}

#[test]
fn the_sustained_one_request_per_second_is_declared_to_the_limiter() {
    let dblp = Dblp::new().with_limiter(Arc::new(Limiter::new()));
    assert_eq!(
        dblp.limiter().limit_of(SOURCE),
        Some((1, Duration::from_secs(1)))
    );
}

#[tokio::test]
async fn the_service_runs_the_adapter_end_to_end_including_dblp_ids() {
    let (mut server, dblp) = adapter().await;
    let _s = server
        .mock("GET", "/search/publ/api")
        .match_query(Matcher::UrlEncoded("q".into(), "attention".into()))
        .with_status(200)
        .with_body(fixture("search.json"))
        .create_async()
        .await;
    let _f = server
        .mock("GET", "/rec/conf/nips/VaswaniSPUJGKP17.xml")
        .with_status(200)
        .with_body(fixture("rec.xml"))
        .create_async()
        .await;
    let registry = Registry::new();
    registry.register(Arc::new(dblp));
    let service = DefaultSourceService::with_registry(registry);
    let explain = service.explain(None).await.answer.unwrap();
    assert_eq!(explain.sources[0].source, "dblp");
    let answer = service
        .search(
            Query::clause("any", "any", "attention"),
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
    assert!(answer.next.unwrap().contains("dblp"));
    let r = service
        .fetch(format!("https://dblp.org/rec/{KEY}.html"), None, None)
        .await;
    assert_eq!(r.answer.unwrap().work.ids["dblp"], KEY);
}

/// Live, opt-in: `cargo test -p source-dblp -- --ignored live`. Needs the
/// network, and dblp.org must not be answering with its bot check (on
/// 2026-10-19 it was; the test then fails with that message, by design).
#[tokio::test]
#[ignore = "reaches dblp.org"]
async fn live_search_and_fetch() {
    let dblp = Dblp::new().with_limiter(Arc::new(Limiter::new()));
    let out = dblp
        .search(
            &Query::clause("any", "any", "attention is all you need vaswani"),
            page(3),
        )
        .await
        .unwrap();
    assert!(!out.rows.is_empty());
    assert!(out.rows.iter().all(|r| r.ids.contains_key("dblp")));
    let work = dblp
        .fetch(&format!("dblp:{KEY}"))
        .await
        .unwrap()
        .expect("the Transformer record exists");
    assert_eq!(work.title.as_deref(), Some("Attention is All you Need"));
    let bib = dblp
        .export(&[format!("dblp:{KEY}")], ExportFormat::Bibtex)
        .await
        .unwrap();
    assert!(bib.contains("DBLP:conf/nips/VaswaniSPUJGKP17"));
}
