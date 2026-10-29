//! A Crossref `work` message → `bibliographic-work@1` (S-D4): the CSL
//! fields, `ids.doi` (and the arXiv id when a relation names one),
//! `locations[]` from `link[]` with DRIVER's `version` from
//! `content-version` (vor → published, am → accepted, else submitted),
//! `provenance`, and `metrics.cited_by` from `is-referenced-by-count`.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

/// Crossref's `type` → CSL `type`.
fn csl_type(crossref: &str) -> &str {
    match crossref {
        "journal-article" => "article-journal",
        "proceedings-article" => "paper-conference",
        "book-chapter" | "book-section" | "book-part" => "chapter",
        "monograph" | "edited-book" | "reference-book" | "book" => "book",
        "posted-content" => "article",
        "dissertation" => "thesis",
        "report" | "report-component" => "report",
        "dataset" => "dataset",
        "journal" | "journal-issue" | "journal-volume" => "periodical",
        other => other,
    }
}

/// DRIVER's version from Crossref's `content-version`.
pub fn version_of(content_version: Option<&str>) -> Version {
    match content_version {
        Some("vor") => Version::Published,
        Some("am") => Version::Accepted,
        _ => Version::Submitted,
    }
}

fn first_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn date_parts(v: Option<&Value>) -> Option<Issued> {
    let parts = v?.get("date-parts")?.as_array()?;
    let date_parts: Vec<Vec<i32>> = parts
        .iter()
        .filter_map(|p| {
            let inner: Vec<i32> = p
                .as_array()?
                .iter()
                .filter_map(|n| n.as_i64().map(|n| n as i32))
                .collect();
            (!inner.is_empty()).then_some(inner)
        })
        .collect();
    (!date_parts.is_empty()).then_some(Issued { date_parts })
}

/// Strip JATS tags from an abstract (`<jats:p>…</jats:p>`), keeping the text.
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

/// Strip the inline tags publishers deposit in a title (`<i>`, `<sub>`,
/// `<scp>`, `<mml:math>`), keeping their text. Unlike [`strip_tags`] a `<`
/// that does not open a tag (`z < 6`) is text: a tag starts `<` or `</`
/// followed by a letter and ends at the next `>`.
fn strip_inline_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let name = after.strip_prefix('/').unwrap_or(after);
        match (name.chars().next(), after.find('>')) {
            (Some(c), Some(end)) if c.is_ascii_alphabetic() => rest = &after[end + 1..],
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse one `message` of `message-type: work` (or one item of a
/// `work-list`). `None` when it has no DOI, which no Crossref work lacks.
pub fn work(item: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let doi = item.get("DOI")?.as_str()?.to_string();
    let mut ids = BTreeMap::from([("doi".to_string(), doi.clone())]);
    // A posted-content record whose relation names its arXiv id.
    if let Some(rels) = item.get("relation").and_then(Value::as_object) {
        for (_, list) in rels {
            for rel in list.as_array().into_iter().flatten() {
                if let (Some("arxiv"), Some(id)) = (
                    rel.get("id-type").and_then(Value::as_str),
                    rel.get("id").and_then(Value::as_str),
                ) {
                    ids.insert("arxiv".into(), id.to_string());
                }
            }
        }
    }
    let author = item
        .get("author")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|a| Author {
                    family: a
                        .get("family")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| a.get("name").and_then(Value::as_str).map(str::to_string)),
                    given: a.get("given").and_then(Value::as_str).map(str::to_string),
                    orcid: a
                        .get("ORCID")
                        .and_then(Value::as_str)
                        .map(|o| o.rsplit('/').next().unwrap_or(o).to_string()),
                })
                .collect()
        })
        .unwrap_or_default();
    let license = item
        .get("license")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .and_then(|l| l.get("URL"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let landing = item.get("URL").and_then(Value::as_str).map(str::to_string);
    let mut locations = vec![Location {
        source: SOURCE.into(),
        source_id: doi.clone(),
        version: Version::Published,
        url: landing.clone(),
        pdf: None,
        license: license.clone(),
        oa_status: None,
        primary: true,
    }];
    for link in item
        .get("link")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(url) = link.get("URL").and_then(Value::as_str) else {
            continue;
        };
        let version = version_of(link.get("content-version").and_then(Value::as_str));
        let content_type = link
            .get("content-type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let is_pdf = content_type == "application/pdf" || url.to_ascii_lowercase().contains("pdf");
        // The landing page's own location gets the pdf; another version is
        // a location of its own.
        if version == Version::Published {
            if is_pdf && locations[0].pdf.is_none() {
                locations[0].pdf = Some(url.to_string());
            }
            continue;
        }
        if locations.iter().any(|l| l.version == version) {
            continue;
        }
        locations.push(Location {
            source: SOURCE.into(),
            source_id: doi.clone(),
            version,
            url: Some(url.to_string()),
            pdf: is_pdf.then(|| url.to_string()),
            license: license.clone(),
            oa_status: None,
            primary: false,
        });
    }
    let issued = date_parts(item.get("issued"))
        .or_else(|| date_parts(item.get("published")))
        .or_else(|| date_parts(item.get("created")));
    Some(Work {
        ty: item
            .get("type")
            .and_then(Value::as_str)
            .map(|t| csl_type(t).to_string())
            .unwrap_or_else(|| "article".into()),
        title: first_string(item.get("title")).map(|t| strip_inline_tags(&t)),
        author,
        issued,
        container_title: first_string(item.get("container-title")),
        volume: item
            .get("volume")
            .and_then(Value::as_str)
            .map(str::to_string),
        issue: item
            .get("issue")
            .and_then(Value::as_str)
            .map(str::to_string),
        page: item.get("page").and_then(Value::as_str).map(str::to_string),
        abstract_: item.get("abstract").and_then(Value::as_str).map(strip_tags),
        language: item
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_string),
        doi: Some(doi.clone()),
        url: landing,
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: doi,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
            sources: Vec::new(),
        }),
        metrics: item
            .get("is-referenced-by-count")
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

/// A `reference[]` entry of a work → a minimal work for `related cites`:
/// the DOI when Crossref matched one, else what the citing record wrote.
pub fn reference(entry: &Value, fetched_at: &str) -> Option<Work> {
    let doi = entry.get("DOI").and_then(Value::as_str)?.to_string();
    let year = entry
        .get("year")
        .and_then(Value::as_str)
        .and_then(|y| y.parse::<i32>().ok());
    Some(Work {
        ty: "article-journal".into(),
        title: entry
            .get("article-title")
            .or_else(|| entry.get("volume-title"))
            .or_else(|| entry.get("unstructured"))
            .and_then(Value::as_str)
            .map(str::to_string),
        author: entry
            .get("author")
            .and_then(Value::as_str)
            .map(|a| {
                vec![Author {
                    family: Some(a.to_string()),
                    ..Default::default()
                }]
            })
            .unwrap_or_default(),
        issued: year.map(|y| Issued {
            date_parts: vec![vec![y]],
        }),
        container_title: entry
            .get("journal-title")
            .and_then(Value::as_str)
            .map(str::to_string),
        volume: entry
            .get("volume")
            .and_then(Value::as_str)
            .map(str::to_string),
        page: entry
            .get("first-page")
            .and_then(Value::as_str)
            .map(str::to_string),
        doi: Some(doi.clone()),
        url: Some(format!("https://doi.org/{doi}")),
        ids: BTreeMap::from([("doi".to_string(), doi.clone())]),
        locations: vec![Location {
            source: SOURCE.into(),
            source_id: doi.clone(),
            version: Version::Published,
            url: Some(format!("https://doi.org/{doi}")),
            pdf: None,
            license: None,
            oa_status: None,
            primary: true,
        }],
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: doi,
            query: None,
            sources: Vec::new(),
        }),
        ..Default::default()
    })
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn a_title_loses_its_inline_tags_and_keeps_a_bare_less_than() {
        let item = serde_json::json!({
            "DOI": "10.1/x",
            "title": ["The <i>z</i> &lt; 6 sky: H<sub>2</sub> at z < 6 in <scp>Gaia</scp>"],
        });
        let work = work(&item, "2026-10-28T00:00:00Z", None).unwrap();
        assert_eq!(
            work.title.as_deref(),
            Some("The z &lt; 6 sky: H2 at z < 6 in Gaia")
        );
        assert_eq!(strip_inline_tags("a <b"), "a <b");
        assert_eq!(strip_inline_tags("1 <2> 3"), "1 <2> 3");
    }
}
