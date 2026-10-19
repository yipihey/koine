//! An esummary document (`retmode=json`) → `bibliographic-work@1` (S-D4):
//! the CSL fields, `ids` pmid/pmcid/doi, `locations[]` (the published
//! record at the DOI or PubMed, plus the PMC copy when a pmcid exists),
//! `provenance`, and `metrics.cited_by` from `pmcrefcount` — the number of
//! PubMed Central articles citing the work, a floor rather than the total,
//! so it is labelled with its own source name (`pubmed-pmc`), not `pubmed`.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

/// The source name `metrics.cited_by` carries (see the module doc).
pub const CITED_BY_SOURCE: &str = "pubmed-pmc";

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// `Lin S` → family `Lin`, given `S`: esummary has only initials, last. A
/// name whose last token is not initials (a collective, `Cas9 Consortium`)
/// stays whole in `family`.
pub fn author(name: &str, authtype: Option<&str>) -> Author {
    let name = name.trim();
    if authtype == Some("CollectiveName") {
        return Author {
            family: Some(name.to_string()),
            ..Default::default()
        };
    }
    match name.rsplit_once(' ') {
        Some((family, initials))
            if (1..=4).contains(&initials.len())
                && initials
                    .chars()
                    .all(|c| c.is_alphabetic() && c.is_uppercase()) =>
        {
            Author {
                family: Some(family.to_string()),
                given: Some(initials.to_string()),
                orcid: None,
            }
        }
        _ => Author {
            family: Some(name.to_string()),
            ..Default::default()
        },
    }
}

fn month(name: &str) -> Option<i32> {
    const M: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = name.to_ascii_lowercase();
    M.iter()
        .position(|m| lower.starts_with(m))
        .map(|i| i as i32 + 1)
}

/// `2014 Dec 15`, `2002 Jan`, `2014`, `2014 Winter` → CSL date parts.
pub fn issued(pubdate: &str) -> Option<Issued> {
    let mut parts = pubdate.split_whitespace();
    let year: i32 = parts.next()?.parse().ok()?;
    let mut date = vec![year];
    if let Some(m) = parts.next().and_then(month) {
        date.push(m);
        if let Some(d) = parts.next().and_then(|d| d.parse::<i32>().ok()) {
            date.push(d);
        }
    }
    Some(Issued {
        date_parts: vec![date],
    })
}

fn article_id(item: &Value, idtype: &str) -> Option<String> {
    item.get("articleids")?
        .as_array()?
        .iter()
        .find(|a| a.get("idtype").and_then(Value::as_str) == Some(idtype))
        .and_then(|a| s(a, "value"))
}

/// Parse one esummary entry. `None` when it is an error stub
/// (`"error": "cannot get document summary"`) or has no uid.
pub fn work(item: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    if item.get("error").is_some() {
        return None;
    }
    let pmid = s(item, "uid")?;
    let doi = article_id(item, "doi");
    let pmcid = article_id(item, "pmc");
    let mut ids = BTreeMap::from([("pmid".to_string(), pmid.clone())]);
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    if let Some(p) = &pmcid {
        ids.insert("pmcid".into(), p.clone());
    }
    let landing = format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/");
    let mut locations = vec![Location {
        source: SOURCE.into(),
        source_id: pmid.clone(),
        version: Version::Published,
        url: Some(
            doi.as_ref()
                .map(|d| format!("https://doi.org/{d}"))
                .unwrap_or_else(|| landing.clone()),
        ),
        pdf: None,
        license: None,
        oa_status: None,
        primary: true,
    }];
    if let Some(pmc) = &pmcid {
        // esummary does not say which version PMC holds; the author
        // manuscript is the commonest deposit, so the honest DRIVER label
        // is `accepted` (fixtures/README.md).
        locations.push(Location {
            source: SOURCE.into(),
            source_id: pmid.clone(),
            version: Version::Accepted,
            url: Some(format!("https://pmc.ncbi.nlm.nih.gov/articles/{pmc}/")),
            pdf: Some(format!("https://pmc.ncbi.nlm.nih.gov/articles/{pmc}/pdf/")),
            license: None,
            oa_status: None,
            primary: false,
        });
    }
    let title = s(item, "title").map(|t| t.trim_end_matches('.').to_string());
    let authors = item
        .get("authors")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(author(
                        a.get("name")?.as_str()?,
                        a.get("authtype").and_then(Value::as_str),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Work {
        ty: "article-journal".into(),
        title,
        author: authors,
        issued: s(item, "pubdate")
            .and_then(|d| issued(&d))
            .or_else(|| s(item, "epubdate").and_then(|d| issued(&d))),
        container_title: s(item, "fulljournalname").or_else(|| s(item, "source")),
        volume: s(item, "volume"),
        issue: s(item, "issue"),
        page: s(item, "pages"),
        abstract_: None,
        language: item
            .get("lang")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(Value::as_str)
            .map(str::to_string),
        doi: doi.clone(),
        url: Some(landing),
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: pmid,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
            sources: Vec::new(),
        }),
        metrics: item
            .get("pmcrefcount")
            .and_then(Value::as_u64)
            .map(|n| Metrics {
                cited_by: Some(Counted {
                    value: n,
                    source: CITED_BY_SOURCE.into(),
                }),
                cited_by_all: Vec::new(),
            }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_dates() {
        let a = author("Lin S", None);
        assert_eq!(
            (a.family.as_deref(), a.given.as_deref()),
            (Some("Lin"), Some("S"))
        );
        let a = author("van der Waals JD", None);
        assert_eq!(a.family.as_deref(), Some("van der Waals"));
        let a = author("CRISPR Consortium", Some("CollectiveName"));
        assert_eq!(a.family.as_deref(), Some("CRISPR Consortium"));
        assert_eq!(author("Madonna", None).given, None);
        assert_eq!(
            issued("2014 Dec 15").unwrap().date_parts,
            vec![vec![2014, 12, 15]]
        );
        assert_eq!(issued("2002 Jan").unwrap().date_parts, vec![vec![2002, 1]]);
        assert_eq!(issued("2014 Winter").unwrap().date_parts, vec![vec![2014]]);
        assert_eq!(issued("soon"), None);
    }
}
