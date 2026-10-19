//! An ADS search `doc` → `bibliographic-work@1` (S-D4): the CSL fields,
//! `ids` bibcode / doi / arxiv, `locations[]` from `esources` and
//! `property` (published versus eprint, each with the ADS link-gateway
//! url), and `metrics.cited_by` from `citation_count`.
//!
//! The doc is read as JSON, so a field ADS omits is simply absent. Not
//! mapped: `read_count`, `keyword`, `arxiv_class` and the alternate
//! bibcodes in `identifier` have no field in the work kind; `AUTHOR_*`
//! esources (author-posted copies) are not turned into a location, since
//! ADS does not say which version they are; the abstract's `<SUB>`/`<SUP>`
//! markup is stripped to text.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

/// The fields every request asks for (`fl`).
pub const FIELDS: &str = "bibcode,title,author,year,pubdate,pub,pub_raw,bibstem,volume,issue,page,page_range,doi,abstract,citation_count,read_count,property,esources,identifier,arxiv_class,keyword,doctype,orcid_pub";

/// ADS's `doctype` → CSL `type`.
fn csl_type(doctype: &str) -> &str {
    match doctype {
        "article" | "erratum" | "obituary" | "newsletter" | "bookreview" => "article-journal",
        "eprint" | "abstract" | "pressrelease" | "talk" | "circular" => "article",
        "inproceedings" | "proceedings" => "paper-conference",
        "book" | "inbook" => "book",
        "phdthesis" | "mastersthesis" => "thesis",
        "techreport" => "report",
        "software" => "software",
        "catalog" | "dataset" => "dataset",
        _ => "article",
    }
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn first(v: &Value, key: &str) -> Option<String> {
    strings(v, key).into_iter().next().filter(|s| !s.is_empty())
}

fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Abel, Tom` → family `Abel`, given `Tom`; no comma is a family name.
fn author(name: &str, orcid: Option<&str>) -> Author {
    let orcid = orcid
        .filter(|o| !o.is_empty() && *o != "-")
        .map(str::to_string);
    match name.split_once(',') {
        Some((family, given)) => Author {
            family: Some(family.trim().to_string()),
            given: Some(given.trim().to_string()).filter(|g| !g.is_empty()),
            orcid,
        },
        None => Author {
            family: Some(name.trim().to_string()),
            given: None,
            orcid,
        },
    }
}

/// `2002-01-00` → `[[2002, 1]]`; a zero month or day is absent.
fn issued(doc: &Value) -> Option<Issued> {
    if let Some(date) = doc.get("pubdate").and_then(Value::as_str) {
        let mut parts: Vec<i32> = Vec::new();
        for p in date.split('-') {
            match p.parse::<i32>() {
                Ok(n) if n > 0 => parts.push(n),
                _ => break,
            }
        }
        if !parts.is_empty() {
            return Some(Issued {
                date_parts: vec![parts],
            });
        }
    }
    doc.get("year")
        .and_then(Value::as_str)
        .and_then(|y| y.parse::<i32>().ok())
        .map(|y| Issued {
            date_parts: vec![vec![y]],
        })
}

/// The ADS link-gateway url of one esource type for a bibcode.
pub fn gateway(bibcode: &str, kind: &str) -> String {
    format!(
        "https://ui.adsabs.harvard.edu/link_gateway/{}/{kind}",
        urlencoding::encode(bibcode)
    )
}

/// The abstract page of a bibcode.
pub fn abstract_page(bibcode: &str) -> String {
    format!(
        "https://ui.adsabs.harvard.edu/abs/{}/abstract",
        urlencoding::encode(bibcode)
    )
}

/// `locations[]` from `esources` and `property`.
fn locations(
    bibcode: &str,
    doctype: &str,
    esources: &[String],
    property: &[String],
) -> Vec<Location> {
    let has = |list: &[String], k: &str| list.iter().any(|e| e == k);
    let pub_html = has(esources, "PUB_HTML");
    let pub_pdf = has(esources, "PUB_PDF");
    let eprint_html = has(esources, "EPRINT_HTML");
    let eprint_pdf = has(esources, "EPRINT_PDF");
    let mut out = Vec::new();
    let eprint_only_doc = doctype == "eprint";
    if pub_html || pub_pdf || (!eprint_only_doc && !(eprint_html || eprint_pdf)) {
        out.push(Location {
            source: SOURCE.into(),
            source_id: bibcode.into(),
            version: Version::Published,
            url: Some(if pub_html {
                gateway(bibcode, "PUB_HTML")
            } else if pub_pdf {
                gateway(bibcode, "PUB_PDF")
            } else {
                abstract_page(bibcode)
            }),
            pdf: pub_pdf.then(|| gateway(bibcode, "PUB_PDF")),
            license: None,
            oa_status: has(property, "PUB_OPENACCESS").then(|| "gold".to_string()),
            primary: false,
        });
    }
    if eprint_html || eprint_pdf || eprint_only_doc {
        out.push(Location {
            source: SOURCE.into(),
            source_id: bibcode.into(),
            version: Version::Submitted,
            url: Some(if eprint_html {
                gateway(bibcode, "EPRINT_HTML")
            } else if eprint_pdf {
                gateway(bibcode, "EPRINT_PDF")
            } else {
                abstract_page(bibcode)
            }),
            pdf: eprint_pdf.then(|| gateway(bibcode, "EPRINT_PDF")),
            license: None,
            oa_status: (has(property, "EPRINT_OPENACCESS") || has(property, "OPENACCESS"))
                .then(|| "green".to_string()),
            primary: false,
        });
    }
    if let Some(l) = out.first_mut() {
        l.primary = true;
    }
    out
}

/// Parse one `doc`. `None` when it has no bibcode.
pub fn work(doc: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let bibcode = doc.get("bibcode")?.as_str()?.to_string();
    let mut ids = BTreeMap::from([("bibcode".to_string(), bibcode.clone())]);
    // ADS lists the arXiv DOI beside the publisher's; the work's DOI is the
    // publisher's.
    let doi = strings(doc, "doi")
        .into_iter()
        .find(|d| !d.to_ascii_lowercase().starts_with("10.48550/arxiv"));
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    if let Some(a) = strings(doc, "identifier")
        .iter()
        .find_map(|i| i.strip_prefix("arXiv:").map(str::to_string))
    {
        ids.insert("arxiv".into(), a);
    }
    let doctype = doc
        .get("doctype")
        .and_then(Value::as_str)
        .unwrap_or("article");
    let orcids = strings(doc, "orcid_pub");
    let authors: Vec<Author> = strings(doc, "author")
        .iter()
        .enumerate()
        .map(|(i, a)| author(a, orcids.get(i).map(String::as_str)))
        .collect();
    // An e-print's `page` is its own id (`arXiv:1706.03762`), not a page.
    let page = if doctype == "eprint" {
        None
    } else {
        first(doc, "page_range").or_else(|| first(doc, "page"))
    };
    Some(Work {
        ty: csl_type(doctype).into(),
        title: first(doc, "title"),
        author: authors,
        issued: issued(doc),
        // An e-print's `pub` is "arXiv e-prints": a repository, not a container.
        container_title: if doctype == "eprint" {
            None
        } else {
            doc.get("pub").and_then(Value::as_str).map(str::to_string)
        },
        volume: doc
            .get("volume")
            .and_then(Value::as_str)
            .map(str::to_string),
        issue: doc.get("issue").and_then(Value::as_str).map(str::to_string),
        page,
        abstract_: doc
            .get("abstract")
            .and_then(Value::as_str)
            .map(strip_tags)
            .filter(|a| !a.is_empty()),
        language: None,
        doi,
        url: Some(abstract_page(&bibcode)),
        ids,
        locations: locations(
            &bibcode,
            doctype,
            &strings(doc, "esources"),
            &strings(doc, "property"),
        ),
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: bibcode,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
        }),
        metrics: doc
            .get("citation_count")
            .and_then(Value::as_u64)
            .map(|n| Metrics {
                cited_by: Some(Counted {
                    value: n,
                    source: SOURCE.into(),
                }),
            }),
    })
}
