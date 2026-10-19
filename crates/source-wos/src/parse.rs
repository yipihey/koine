//! A Web of Science Starter API document (`hits[]` of a search, or the body
//! of `GET /documents/{uid}`) → `bibliographic-work@1` (S-D4): the CSL
//! fields, `ids.wos` (the UT, `WOS:` prefix kept), `ids.doi`/`ids.pmid` from
//! `identifiers`, `locations[]` (the published record at the DOI, else the
//! WoS record page), `provenance`, and `metrics.cited_by` from
//! `citations[].count` — the Web of Science Core Collection count, which is
//! the `db: "WOS"` entry when several are listed.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

fn s(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(t) => Some(t.trim().to_string()).filter(|t| !t.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// WoS `types[]` → CSL `type` (the first type decides).
pub fn csl_type(types: &[String]) -> &'static str {
    match types.first().map(|t| t.to_ascii_lowercase()).as_deref() {
        Some("proceedings paper") | Some("meeting abstract") => "paper-conference",
        Some("book chapter") => "chapter",
        Some("book") => "book",
        Some("data set") => "dataset",
        _ => "article-journal",
    }
}

/// `Abel, T` / `Abel, Tom` → family and given; a name without a comma (a
/// group author) stays whole in `family`.
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

fn month(name: &str) -> Option<i32> {
    const M: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    let upper = name.to_ascii_uppercase();
    M.iter()
        .position(|m| upper.starts_with(m))
        .map(|i| i as i32 + 1)
}

/// The UT as the canonical id value: always `WOS:` + the 15 characters.
pub fn ut(uid: &str) -> String {
    if uid.len() >= 4 && uid[..4].eq_ignore_ascii_case("WOS:") {
        format!("WOS:{}", &uid[4..])
    } else {
        format!("WOS:{uid}")
    }
}

/// Parse one document. `None` when it has no `uid`.
pub fn work(item: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let uid = ut(&s(item, "uid")?);
    let identifiers = item.get("identifiers").cloned().unwrap_or(Value::Null);
    let doi = s(&identifiers, "doi");
    let mut ids = BTreeMap::from([("wos".to_string(), uid.clone())]);
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    if let Some(p) = s(&identifiers, "pmid") {
        ids.insert("pmid".into(), p);
    }
    let record = item
        .get("links")
        .and_then(|l| s(l, "record"))
        .unwrap_or_else(|| format!("https://www.webofscience.com/wos/woscc/full-record/{uid}"));
    let types: Vec<String> = item
        .get("types")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let source = item.get("source").cloned().unwrap_or(Value::Null);
    let year = source
        .get("publishYear")
        .and_then(|y| {
            y.as_i64()
                .or_else(|| y.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|y| y as i32);
    let issued = year.map(|y| {
        let mut parts = vec![y];
        if let Some(m) = s(&source, "publishMonth").as_deref().and_then(month) {
            parts.push(m);
        }
        Issued {
            date_parts: vec![parts],
        }
    });
    let authors = item
        .get("names")
        .and_then(|n| n.get("authors"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let name = s(a, "wosStandard").or_else(|| s(a, "displayName"))?;
                    let mut au = author(&name);
                    au.orcid =
                        s(a, "orcid").map(|o| o.rsplit('/').next().unwrap_or(&o).to_string());
                    Some(au)
                })
                .collect()
        })
        .unwrap_or_default();
    let cited = item
        .get("citations")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .find(|c| c.get("db").and_then(Value::as_str) == Some("WOS"))
                .or_else(|| list.first())
        })
        .and_then(|c| c.get("count").and_then(Value::as_u64));
    Some(Work {
        ty: csl_type(&types).into(),
        title: s(item, "title"),
        author: authors,
        issued,
        container_title: s(&source, "sourceTitle"),
        volume: s(&source, "volume"),
        issue: s(&source, "issue"),
        page: source.get("pages").and_then(|p| s(p, "range")),
        abstract_: None,
        language: None,
        doi: doi.clone(),
        url: Some(record.clone()),
        ids,
        locations: vec![Location {
            source: SOURCE.into(),
            source_id: uid.clone(),
            version: Version::Published,
            url: Some(
                doi.as_ref()
                    .map(|d| format!("https://doi.org/{d}"))
                    .unwrap_or(record),
            ),
            pdf: None,
            license: None,
            oa_status: None,
            primary: true,
        }],
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: uid,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
        }),
        metrics: cited.map(|n| Metrics {
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
    fn names_uts_types() {
        let a = author("Abel, T");
        assert_eq!(
            (a.family.as_deref(), a.given.as_deref()),
            (Some("Abel"), Some("T"))
        );
        assert_eq!(author("ATLAS Collaboration").given, None);
        assert_eq!(ut("000175157300021"), "WOS:000175157300021");
        assert_eq!(ut("wos:000175157300021"), "WOS:000175157300021");
        assert_eq!(
            csl_type(&["Proceedings Paper".into(), "Article".into()]),
            "paper-conference"
        );
        assert_eq!(csl_type(&["Review".into()]), "article-journal");
        assert_eq!(month("JAN-FEB"), Some(1));
        assert_eq!(month("SPR"), None);
    }
}
