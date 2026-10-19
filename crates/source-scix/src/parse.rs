//! An ADS-shaped `docs[]` entry → `bibliographic-work@1` (S-D4): CSL fields,
//! `ids` bibcode/doi/arxiv (the arXiv id from `identifier`), `locations[]`
//! from `esources` + `property` through SciX's link gateway,
//! `provenance`, and `metrics.cited_by` from `citation_count`.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::{ABS_URL, GATEWAY_URL, SOURCE};

/// The `fl` every search and fetch asks for.
pub const FIELDS: &str = "bibcode,title,author,orcid_pub,year,pubdate,pub,volume,issue,page,abstract,doi,identifier,citation_count,esources,property,doctype,lang";

fn strings(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn first(v: &Value, key: &str) -> Option<String> {
    strings(v, key)
        .into_iter()
        .next()
        .filter(|s| !s.trim().is_empty())
}

fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()).filter(|s| !s.trim().is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// ADS `doctype` → CSL `type`.
pub fn csl_type(doctype: &str) -> &'static str {
    match doctype {
        "article" => "article-journal",
        "inproceedings" | "abstract" => "paper-conference",
        "book" | "proceedings" => "book",
        "inbook" => "chapter",
        "phdthesis" | "mastersthesis" => "thesis",
        "techreport" => "report",
        "software" => "software",
        "dataset" => "dataset",
        "eprint" => "article",
        _ => "article",
    }
}

/// `Abel, Tom` → family and given.
pub fn author(name: &str) -> Author {
    match name.split_once(',') {
        Some((family, given)) if !family.trim().is_empty() => Author {
            family: Some(family.trim().to_string()),
            given: Some(given.trim().to_string()).filter(|g| !g.is_empty()),
            orcid: None,
        },
        _ => Author {
            family: Some(name.trim().to_string()),
            ..Default::default()
        },
    }
}

/// `2002-01-00` → `[2002, 1]`: a `00` month or day is unknown, not zero.
pub fn issued(pubdate: Option<&str>, year: Option<&str>) -> Option<Issued> {
    let mut parts: Vec<i32> = Vec::new();
    if let Some(d) = pubdate {
        for p in d.split('-') {
            match p.parse::<i32>() {
                Ok(n) if n > 0 => parts.push(n),
                _ => break,
            }
        }
    }
    if parts.is_empty() {
        parts.push(year?.parse().ok()?);
    }
    Some(Issued {
        date_parts: vec![parts],
    })
}

/// The arXiv id in `identifier[]`: `arXiv:astro-ph/0112088` or `arXiv:2101.00001`.
pub fn arxiv_of(identifiers: &[String]) -> Option<String> {
    identifiers.iter().find_map(|i| {
        i.get(..6)
            .filter(|p| p.eq_ignore_ascii_case("arxiv:"))
            .map(|_| i[6..].to_string())
    })
}

fn gateway(bibcode: &str, kind: &str) -> String {
    format!("{GATEWAY_URL}/{bibcode}/{kind}")
}

/// Parse one document. `None` when it has no bibcode.
pub fn work(item: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let bibcode = text(item, "bibcode")?;
    let identifiers = strings(item, "identifier");
    let doi = first(item, "doi");
    let arxiv = arxiv_of(&identifiers);
    let mut ids = BTreeMap::from([("bibcode".to_string(), bibcode.clone())]);
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    if let Some(a) = &arxiv {
        ids.insert("arxiv".into(), a.clone());
    }
    let esources = strings(item, "esources");
    let properties = strings(item, "property");
    let has = |k: &str| esources.iter().any(|e| e == k);
    let open = |k: &str| properties.iter().any(|p| p == k);

    let mut locations: Vec<Location> = Vec::new();
    // The published version: publisher HTML/PDF through the gateway, else
    // the DOI; ADS's own scan is the pdf of last resort.
    if has("PUB_HTML") || has("PUB_PDF") || has("ADS_PDF") || doi.is_some() {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: bibcode.clone(),
            version: Version::Published,
            url: Some(if has("PUB_HTML") {
                gateway(&bibcode, "PUB_HTML")
            } else if let Some(d) = &doi {
                format!("https://doi.org/{d}")
            } else {
                format!("{ABS_URL}/{bibcode}/abstract")
            }),
            pdf: if has("PUB_PDF") {
                Some(gateway(&bibcode, "PUB_PDF"))
            } else if has("ADS_PDF") {
                Some(gateway(&bibcode, "ADS_PDF"))
            } else {
                None
            },
            license: None,
            oa_status: open("PUB_OPENACCESS")
                .then(|| "gold".to_string())
                .or_else(|| open("OPENACCESS").then(|| "open".to_string())),
            primary: false,
        });
    }
    if has("AUTHOR_PDF") || has("AUTHOR_HTML") {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: bibcode.clone(),
            version: Version::Accepted,
            url: Some(gateway(
                &bibcode,
                if has("AUTHOR_HTML") {
                    "AUTHOR_HTML"
                } else {
                    "AUTHOR_PDF"
                },
            )),
            pdf: has("AUTHOR_PDF").then(|| gateway(&bibcode, "AUTHOR_PDF")),
            license: None,
            oa_status: Some("green".into()),
            primary: false,
        });
    }
    if let Some(a) = arxiv
        .as_ref()
        .filter(|_| has("EPRINT_PDF") || has("EPRINT_HTML"))
    {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: bibcode.clone(),
            version: Version::Submitted,
            url: Some(format!("https://arxiv.org/abs/{a}")),
            pdf: Some(format!("https://arxiv.org/pdf/{a}")),
            license: None,
            oa_status: Some("green".into()),
            primary: false,
        });
    }
    if locations.is_empty() {
        locations.push(Location {
            source: SOURCE.into(),
            source_id: bibcode.clone(),
            version: Version::Published,
            url: Some(format!("{ABS_URL}/{bibcode}/abstract")),
            pdf: None,
            license: None,
            oa_status: None,
            primary: false,
        });
    }
    locations[0].primary = true;

    let orcids = strings(item, "orcid_pub");
    let authors: Vec<Author> = strings(item, "author")
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut a = author(name);
            a.orcid = orcids
                .get(i)
                .filter(|o| !o.is_empty() && o.as_str() != "-")
                .cloned();
            a
        })
        .collect();
    Some(Work {
        ty: text(item, "doctype")
            .map(|d| csl_type(&d).to_string())
            .unwrap_or_else(|| "article".into()),
        title: first(item, "title"),
        author: authors,
        issued: issued(
            text(item, "pubdate").as_deref(),
            text(item, "year").as_deref(),
        ),
        container_title: text(item, "pub"),
        volume: text(item, "volume"),
        issue: text(item, "issue"),
        page: first(item, "page"),
        abstract_: text(item, "abstract"),
        language: first(item, "lang"),
        doi: doi.clone(),
        url: Some(format!("{ABS_URL}/{bibcode}/abstract")),
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: bibcode,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
        }),
        metrics: item
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_names_ids() {
        assert_eq!(
            issued(Some("2002-01-00"), Some("2002")).unwrap().date_parts,
            vec![vec![2002, 1]]
        );
        assert_eq!(
            issued(Some("2002-00-00"), Some("2002")).unwrap().date_parts,
            vec![vec![2002]]
        );
        assert_eq!(
            issued(None, Some("1999")).unwrap().date_parts,
            vec![vec![1999]]
        );
        assert_eq!(issued(None, None), None);
        let a = author("Abel, Tom");
        assert_eq!(
            (a.family.as_deref(), a.given.as_deref()),
            (Some("Abel"), Some("Tom"))
        );
        assert_eq!(
            arxiv_of(&[
                "2002Sci...295...93A".into(),
                "arXiv:astro-ph/0112088".into()
            ])
            .as_deref(),
            Some("astro-ph/0112088")
        );
        assert_eq!(csl_type("inproceedings"), "paper-conference");
    }
}
