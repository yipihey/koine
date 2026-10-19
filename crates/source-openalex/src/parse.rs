//! An OpenAlex `Work` object → `bibliographic-work@1` (S-D4): the CSL
//! fields, `ids` from `ids{doi,openalex,pmid,pmcid,mag}` (plus the arXiv id
//! when a location is arxiv.org), `locations[]` from `primary_location` and
//! `locations[]` with DRIVER's `version`, `is_oa`, `license` and `pdf_url`,
//! the abstract rebuilt from `abstract_inverted_index`, and
//! `metrics.cited_by` from `cited_by_count`.
//!
//! Not mapped: OpenAlex gives an author only as `display_name`, so the
//! family/given split is the last token (a heuristic, as in imbib's old
//! client); topics, concepts, funders, institutions and `fwci` have no field
//! in the work kind.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `https://openalex.org/W2040912209` → `W2040912209`.
pub fn short_id(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

fn strip<'a>(s: &'a str, prefixes: &[&str]) -> &'a str {
    prefixes.iter().find_map(|p| s.strip_prefix(p)).unwrap_or(s)
}

/// OpenAlex `type` → CSL `type`.
fn csl_type(t: &str) -> &str {
    match t {
        "article" | "review" | "letter" | "editorial" | "erratum" | "retraction" => {
            "article-journal"
        }
        "preprint" => "article",
        "book" => "book",
        "book-chapter" => "chapter",
        "dissertation" => "thesis",
        "dataset" => "dataset",
        "report" => "report",
        "standard" => "standard",
        "peer-review" => "review",
        "paratext" | "other" => "document",
        other => other,
    }
}

/// DRIVER's version from a location's `version`, `is_accepted`, `is_published`.
pub fn version_of(loc: &Value) -> Version {
    match str_of(loc, "version") {
        Some("publishedVersion") => Version::Published,
        Some("acceptedVersion") => Version::Accepted,
        Some("submittedVersion") => Version::Submitted,
        _ => {
            if loc.get("is_published").and_then(Value::as_bool) == Some(true) {
                Version::Published
            } else if loc.get("is_accepted").and_then(Value::as_bool) == Some(true) {
                Version::Accepted
            } else {
                Version::Submitted
            }
        }
    }
}

/// OpenAlex stores abstracts as `{word: [positions]}`; put them back.
pub fn abstract_of(index: &serde_json::Map<String, Value>) -> Option<String> {
    let mut words: Vec<(u64, &str)> = Vec::new();
    for (word, positions) in index {
        for p in positions.as_array().into_iter().flatten() {
            if let Some(i) = p.as_u64() {
                words.push((i, word.as_str()));
            }
        }
    }
    if words.is_empty() {
        return None;
    }
    words.sort_by_key(|(i, _)| *i);
    Some(
        words
            .into_iter()
            .map(|(_, w)| w)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn author(name: &str, orcid: Option<&str>) -> Author {
    let parts: Vec<&str> = name.split_whitespace().collect();
    let orcid = orcid.map(|o| strip(o, &["https://orcid.org/", "http://orcid.org/"]).to_string());
    match parts.split_last() {
        Some((family, given)) if !given.is_empty() => Author {
            family: Some((*family).to_string()),
            given: Some(given.join(" ")),
            orcid,
        },
        _ => Author {
            family: Some(name.trim().to_string()),
            given: None,
            orcid,
        },
    }
}

fn issued(item: &Value) -> Option<Issued> {
    if let Some(date) = str_of(item, "publication_date") {
        let parts: Vec<i32> = date.split('-').filter_map(|p| p.parse().ok()).collect();
        if !parts.is_empty() {
            return Some(Issued {
                date_parts: vec![parts],
            });
        }
    }
    item.get("publication_year")
        .and_then(Value::as_i64)
        .map(|y| Issued {
            date_parts: vec![vec![y as i32]],
        })
}

fn arxiv_id_of(url: &str) -> Option<String> {
    let rest = url.split("arxiv.org/abs/").nth(1)?;
    let id = rest.trim_end_matches('/');
    let id = match id.rfind('v') {
        Some(i)
            if i > 0
                && !id[i + 1..].is_empty()
                && id[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &id[..i]
        }
        _ => id,
    };
    (!id.is_empty()).then(|| id.to_string())
}

fn location(
    loc: &Value,
    source_id: &str,
    best_oa: Option<&str>,
    oa_status: Option<&str>,
    primary: bool,
) -> Location {
    let landing = str_of(loc, "landing_page_url").map(str::to_string);
    let is_oa = loc.get("is_oa").and_then(Value::as_bool) == Some(true);
    let is_best = landing.is_some() && landing.as_deref() == best_oa;
    Location {
        source: SOURCE.into(),
        source_id: source_id.into(),
        version: version_of(loc),
        url: landing,
        pdf: str_of(loc, "pdf_url").map(str::to_string),
        license: str_of(loc, "license").map(str::to_string),
        oa_status: is_oa.then(|| {
            if is_best {
                oa_status.unwrap_or("open").to_string()
            } else {
                "open".to_string()
            }
        }),
        primary,
    }
}

/// Parse one work. `None` when it has no OpenAlex id.
pub fn work(item: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let id = short_id(str_of(item, "id")?).to_string();
    let mut ids = BTreeMap::from([("openalex".to_string(), id.clone())]);
    let doi = str_of(item, "doi")
        .map(|d| strip(d, &["https://doi.org/", "http://doi.org/", "doi:"]).to_string());
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    if let Some(raw) = item.get("ids") {
        if let Some(p) = str_of(raw, "pmid") {
            ids.insert(
                "pmid".into(),
                strip(p, &["https://pubmed.ncbi.nlm.nih.gov/"])
                    .trim_end_matches('/')
                    .to_string(),
            );
        }
        if let Some(p) = str_of(raw, "pmcid") {
            let n = p.rsplit('/').next().unwrap_or(p).trim_start_matches("PMC");
            ids.insert("pmcid".into(), format!("PMC{n}"));
        }
        if let Some(m) = str_of(raw, "mag") {
            ids.insert("mag".into(), m.to_string());
        }
    }
    let mut raw_locations: Vec<&Value> = Vec::new();
    if let Some(p) = item.get("primary_location").filter(|p| p.is_object()) {
        raw_locations.push(p);
    }
    for l in item
        .get("locations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let same = |a: &Value, b: &Value| {
            str_of(a, "landing_page_url") == str_of(b, "landing_page_url")
                && str_of(a, "pdf_url") == str_of(b, "pdf_url")
        };
        if !raw_locations.iter().any(|seen| same(seen, l)) {
            raw_locations.push(l);
        }
    }
    for l in &raw_locations {
        if let Some(a) = str_of(l, "landing_page_url").and_then(arxiv_id_of) {
            ids.entry("arxiv".into()).or_insert(a);
        }
    }
    let best_oa = item
        .get("best_oa_location")
        .and_then(|b| str_of(b, "landing_page_url"));
    let oa_status = item.get("open_access").and_then(|o| str_of(o, "oa_status"));
    let locations: Vec<Location> = raw_locations
        .iter()
        .enumerate()
        .map(|(i, l)| location(l, &id, best_oa, oa_status, i == 0))
        .collect();
    let primary = item.get("primary_location").filter(|p| p.is_object());
    let source = primary
        .and_then(|p| p.get("source"))
        .filter(|s| s.is_object());
    let container_title = source
        .filter(|s| str_of(s, "type") != Some("repository"))
        .and_then(|s| str_of(s, "display_name"))
        .map(str::to_string);
    let biblio = item.get("biblio");
    let page = biblio.and_then(|b| {
        let first = str_of(b, "first_page")?;
        match str_of(b, "last_page") {
            Some(last) if last != first => Some(format!("{first}-{last}")),
            _ => Some(first.to_string()),
        }
    });
    let landing = primary
        .and_then(|p| str_of(p, "landing_page_url"))
        .map(str::to_string)
        .or_else(|| doi.as_ref().map(|d| format!("https://doi.org/{d}")));
    Some(Work {
        ty: str_of(item, "type")
            .map(|t| csl_type(t).to_string())
            .unwrap_or_else(|| "article".into()),
        title: str_of(item, "title")
            .or_else(|| str_of(item, "display_name"))
            .map(str::to_string),
        author: item
            .get("authorships")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|a| {
                        let au = a.get("author")?;
                        let name = str_of(au, "display_name")?;
                        Some(author(name, str_of(au, "orcid")))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        issued: issued(item),
        container_title,
        volume: biblio.and_then(|b| str_of(b, "volume")).map(str::to_string),
        issue: biblio.and_then(|b| str_of(b, "issue")).map(str::to_string),
        page,
        abstract_: item
            .get("abstract_inverted_index")
            .and_then(Value::as_object)
            .and_then(abstract_of),
        language: str_of(item, "language").map(str::to_string),
        doi,
        url: landing,
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: id,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
            sources: Vec::new(),
        }),
        metrics: item
            .get("cited_by_count")
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
