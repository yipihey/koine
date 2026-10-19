//! An INSPIRE literature record (`{"id", "metadata": {…}}`, a search hit or a
//! single-record response alike) → `bibliographic-work@1` (S-D4).
//!
//! Choices, where INSPIRE and the canonical record disagree:
//!
//! - `ids.doi` is the first DOI, `ids.arxiv` the first eprint (old-style
//!   ids as written), `ids.bibcode` the `ADS` entry of
//!   `external_system_identifiers` (INSPIRE stores the ADS bibcode there).
//!   The recid (`control_number`) is **not** in `ids`: `inspire` is not a
//!   canonical id kind and `Work::validate` refuses it (see lib.rs). It is
//!   `provenance.source_id`, each location's `source_id`, and the `url`.
//! - `issued` is the publication year when the record has a journal entry,
//!   else `earliest_date` (the preprint date). The `year` index searches
//!   `date`, which is the earliest date, so a paper preprinted in December
//!   and published the next year is found under the preprint year and shown
//!   under the publication year.
//! - `container-title` is INSPIRE's journal abbreviation
//!   (`Mon.Not.Roy.Astron.Soc.`), not the full title: INSPIRE holds no other.
//! - A record with a `collaborations` entry and more than
//!   [`COLLABORATION_AUTHORS`] authors is shown with the collaboration as its
//!   one author (`CMS Collaboration`), as CSL writes a corporate author; a
//!   3,000-name list per row would make a hundred-row page unusable.
//! - `locations[]`: the DOI is the `published` location (`primary` when the
//!   record has one), the arXiv eprint a `submitted` one (abs + pdf), and
//!   each INSPIRE-hosted `documents[]` entry another `submitted` one: INSPIRE
//!   does not say which version a hosted document is, and the plan's
//!   reading is the author's copy.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

/// Above this many authors, a record with a collaboration shows the
/// collaboration instead.
pub const COLLABORATION_AUTHORS: usize = 50;

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::to_string)
        .filter(|x| !x.is_empty())
}

/// INSPIRE's `document_type` → CSL `type`.
fn csl_type(types: &[String], published: bool) -> &'static str {
    match types.first().map(String::as_str) {
        Some("conference paper") => "paper-conference",
        Some("book") | Some("proceedings") => "book",
        Some("book chapter") => "chapter",
        Some("thesis") => "thesis",
        Some("report") => "report",
        _ if published => "article-journal",
        _ => "article",
    }
}

/// `Abel, Tom` → family `Abel`, given `Tom`; a name with no comma is all
/// family (a collaboration, a mononym).
fn author(a: &Value) -> Author {
    let orcid = a
        .get("ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| i.get("schema").and_then(Value::as_str) == Some("ORCID"))
        .and_then(|i| s(i.get("value")));
    let (family, given) = match (s(a.get("last_name")), s(a.get("first_name"))) {
        (Some(l), g) => (Some(l), g),
        _ => match s(a.get("full_name")) {
            Some(full) => match full.split_once(',') {
                Some((f, g)) => (
                    Some(f.trim().to_string()),
                    Some(g.trim().to_string()).filter(|g| !g.is_empty()),
                ),
                None => (Some(full), None),
            },
            None => (None, None),
        },
    };
    Author {
        family,
        given,
        orcid,
    }
}

fn date_parts(date: &str) -> Option<Issued> {
    let parts: Vec<i32> = date
        .split('-')
        .take(3)
        .map(|p| p.parse::<i32>())
        .collect::<Result<_, _>>()
        .ok()?;
    (!parts.is_empty()).then(|| Issued {
        date_parts: vec![parts],
    })
}

fn identifier<'a>(m: &'a Value, schema: &str) -> Option<&'a str> {
    m.get("external_system_identifiers")?
        .as_array()?
        .iter()
        .find(|i| i.get("schema").and_then(Value::as_str) == Some(schema))?
        .get("value")?
        .as_str()
}

/// Parse one hit / record. `None` when it has no recid.
pub fn work(record: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let m = record.get("metadata")?;
    let recid = m
        .get("control_number")
        .and_then(Value::as_u64)
        .map(|n| n.to_string())
        .or_else(|| s(record.get("id")).filter(|i| i.chars().all(|c| c.is_ascii_digit())))?;
    let doi = m
        .get("dois")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .and_then(|d| s(d.get("value")));
    let arxiv = m
        .get("arxiv_eprints")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .and_then(|d| s(d.get("value")));
    let mut ids = BTreeMap::new();
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    if let Some(a) = &arxiv {
        ids.insert("arxiv".into(), a.clone());
    }
    if let Some(b) = identifier(m, "ADS").filter(|b| b.len() == 19) {
        ids.insert("bibcode".into(), b.to_string());
    }

    let pubinfo = m
        .get("publication_info")
        .and_then(Value::as_array)
        .and_then(|l| {
            l.iter()
                .find(|p| p.get("journal_title").is_some())
                .or_else(|| l.first())
        });
    let published = pubinfo
        .map(|p| p.get("journal_title").is_some())
        .unwrap_or(false);

    let mut author_list: Vec<Author> = m
        .get("authors")
        .and_then(Value::as_array)
        .map(|l| l.iter().map(author).collect())
        .unwrap_or_default();
    if author_list.len() > COLLABORATION_AUTHORS {
        let collaborations: Vec<Author> = m
            .get("collaborations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| s(c.get("value")))
            .map(|name| Author {
                family: Some(if name.to_ascii_lowercase().contains("collab") {
                    name
                } else {
                    format!("{name} Collaboration")
                }),
                ..Default::default()
            })
            .collect();
        if !collaborations.is_empty() {
            author_list = collaborations;
        }
    }

    let issued = pubinfo
        .and_then(|p| p.get("year").and_then(Value::as_i64))
        .map(|y| Issued {
            date_parts: vec![vec![y as i32]],
        })
        .or_else(|| s(m.get("earliest_date")).and_then(|d| date_parts(&d)))
        .or_else(|| s(m.get("preprint_date")).and_then(|d| date_parts(&d)));

    let page = pubinfo.and_then(|p| match (s(p.get("page_start")), s(p.get("page_end"))) {
        (Some(a), Some(b)) => Some(format!("{a}-{b}")),
        (Some(a), None) => Some(a),
        _ => s(p.get("artid")),
    });

    let landing = format!("https://inspirehep.net/literature/{recid}");
    let mut locations: Vec<Location> = Vec::new();
    if let Some(d) = &doi {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: recid.clone(),
            version: Version::Published,
            url: Some(format!("https://doi.org/{d}")),
            pdf: None,
            license: None,
            oa_status: None,
            primary: true,
        });
    }
    if let Some(a) = &arxiv {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: recid.clone(),
            version: Version::Submitted,
            url: Some(format!("https://arxiv.org/abs/{a}")),
            pdf: Some(format!("https://arxiv.org/pdf/{a}")),
            license: None,
            oa_status: Some("green".into()),
            primary: doi.is_none(),
        });
    }
    for document in m
        .get("documents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(url) = s(document.get("url")) else {
            continue;
        };
        let is_pdf = s(document.get("filename"))
            .map(|f| f.to_ascii_lowercase().ends_with(".pdf"))
            .unwrap_or(false);
        locations.push(Location {
            source: SOURCE.into(),
            source_id: recid.clone(),
            version: Version::Submitted,
            url: Some(url.clone()),
            pdf: is_pdf.then_some(url),
            license: None,
            oa_status: None,
            primary: doi.is_none() && arxiv.is_none() && locations.is_empty(),
        });
    }
    if locations.is_empty() {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: recid.clone(),
            version: Version::Published,
            url: Some(landing.clone()),
            pdf: None,
            license: None,
            oa_status: None,
            primary: true,
        });
    }

    let types: Vec<String> = m
        .get("document_type")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    Some(Work {
        ty: csl_type(&types, published).to_string(),
        title: m
            .get("titles")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(|t| s(t.get("title"))),
        author: author_list,
        issued,
        container_title: pubinfo.and_then(|p| s(p.get("journal_title"))),
        volume: pubinfo.and_then(|p| s(p.get("journal_volume"))),
        issue: pubinfo.and_then(|p| s(p.get("journal_issue"))),
        page,
        abstract_: m
            .get("abstracts")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(|a| s(a.get("value"))),
        language: m
            .get("languages")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(|v| v.as_str().map(str::to_string)),
        doi,
        url: Some(landing),
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: recid,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
            sources: Vec::new(),
        }),
        metrics: m
            .get("citation_count")
            .and_then(Value::as_u64)
            .map(|n| Metrics {
                cited_by: Some(Counted {
                    value: n,
                    source: SOURCE.into(),
                }),
                cited_by_all: Vec::new(),
            }),
    })
}
