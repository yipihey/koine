//! A DBLP record → `bibliographic-work@1` (S-D4), from either shape DBLP
//! answers in: a search hit (`info{…}` of `search/publ/api?format=json`) or a
//! record (`rec/<key>.xml`). Both become one [`Record`] and one [`work`].
//!
//! Choices:
//!
//! - `ids.dblp` is the key (`conf/nips/VaswaniSPUJGKP17`); `ids.doi` comes
//!   from a `doi` field or any `ee` that is a `doi.org` URL; `ids.arxiv` from
//!   an `ee` on arxiv.org.
//! - **Locations** are the `ee` URLs, in order, the first `primary`. A
//!   doi.org URL or any `ee` of a conference/journal record is `published`; an
//!   arxiv.org `ee`, or any `ee` of a `CoRR` (informal) record, is
//!   `submitted`. DBLP says nothing about licences.
//! - **Container** is `venue` (JSON) or `booktitle`/`journal` (XML): DBLP's
//!   abbreviation (`NIPS`, `J. Mach. Learn. Res.`), not a full title.
//! - **Authors**: DBLP's homonym suffix (`David M. Blei 0001`) is removed; the
//!   family/given split is the last token (with a preceding particle such as
//!   `van`/`de` joined to it), a heuristic as in OpenAlex's adapter.
//! - **Title**: DBLP's trailing full stop is removed.
//! - A `volume` of the form `abs/…` (CoRR's) is dropped.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Issued, Location, Provenance, Version};
use koine_sources::{Query, Work};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::Value;

use crate::SOURCE;

/// What both DBLP shapes reduce to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// The CSL type.
    pub ty: String,
    pub key: String,
    pub authors: Vec<String>,
    pub title: Option<String>,
    pub venue: Option<String>,
    pub volume: Option<String>,
    pub number: Option<String>,
    pub pages: Option<String>,
    pub year: Option<i32>,
    pub ee: Vec<String>,
    pub doi: Option<String>,
}

const PARTICLES: &[&str] = &[
    "van", "von", "de", "der", "den", "del", "della", "di", "da", "dos", "du", "la", "le", "bin",
    "ibn", "al", "el", "ter", "ten",
];

/// `David M. Blei 0001` → `David M. Blei`.
fn clean_name(name: &str) -> String {
    let name = name.trim();
    match name.rsplit_once(' ') {
        Some((rest, tail)) if tail.len() == 4 && tail.chars().all(|c| c.is_ascii_digit()) => {
            rest.trim().to_string()
        }
        _ => name.to_string(),
    }
}

fn author(name: &str) -> Author {
    let parts: Vec<&str> = name.split_whitespace().collect();
    if parts.len() < 2 {
        return Author {
            family: (!name.trim().is_empty()).then(|| name.trim().to_string()),
            ..Default::default()
        };
    }
    let mut start = parts.len() - 1;
    while start > 1 && PARTICLES.contains(&parts[start - 1].to_ascii_lowercase().as_str()) {
        start -= 1;
    }
    Author {
        family: Some(parts[start..].join(" ")),
        given: Some(parts[..start].join(" ")),
        orcid: None,
    }
}

fn doi_of(url: &str) -> Option<String> {
    [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
    ]
    .iter()
    .find_map(|p| url.strip_prefix(p))
    .filter(|d| d.starts_with("10.") && d.contains('/'))
    .map(str::to_string)
}

fn arxiv_of(url: &str) -> Option<String> {
    ["https://arxiv.org/abs/", "http://arxiv.org/abs/"]
        .iter()
        .find_map(|p| url.strip_prefix(p))
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
}

fn first_or_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => a.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
    .filter(|s| !s.trim().is_empty())
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn json_type(ty: &str) -> &'static str {
    match ty {
        "Conference and Workshop Papers" => "paper-conference",
        "Journal Articles" => "article-journal",
        "Books and Theses" | "Editorship" | "Reference Works" => "book",
        "Parts in Books or Collections" => "chapter",
        "Data and Artifacts" => "dataset",
        _ => "article",
    }
}

/// One search hit's `info` object.
pub fn record_from_hit(info: &Value) -> Option<Record> {
    let key = info.get("key")?.as_str()?.to_string();
    let authors = match info.get("authors").and_then(|a| a.get("author")) {
        Some(Value::Array(list)) => list.iter().filter_map(author_text).collect(),
        Some(one) => author_text(one).into_iter().collect(),
        None => Vec::new(),
    };
    Some(Record {
        ty: json_type(info.get("type").and_then(Value::as_str).unwrap_or("")).into(),
        key,
        authors,
        title: first_or_string(info.get("title")),
        venue: first_or_string(info.get("venue")),
        volume: first_or_string(info.get("volume")),
        number: first_or_string(info.get("number")),
        pages: first_or_string(info.get("pages")),
        year: first_or_string(info.get("year")).and_then(|y| y.parse().ok()),
        ee: strings(info.get("ee")),
        doi: first_or_string(info.get("doi")),
    })
}

fn author_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        other => other
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// A `rec/<key>.xml` body. `None` when it holds no record.
pub fn record_from_xml(xml: &str) -> Option<Record> {
    let mut reader = Reader::from_str(xml);
    let mut depth = 0usize;
    let mut record = Record::default();
    let mut element = String::new();
    let mut field = String::new();
    let mut text = String::new();
    let mut found = false;
    let mut informal = false;
    let mut editors: Vec<String> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match depth {
                    2 => {
                        element = name;
                        found = true;
                        for a in e.attributes().flatten() {
                            let k = String::from_utf8_lossy(a.key.as_ref()).to_string();
                            let v = a
                                .unescape_value()
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            match k.as_str() {
                                "key" => record.key = v,
                                "publtype" if v.contains("informal") => informal = true,
                                _ => {}
                            }
                        }
                    }
                    3 => {
                        field = name;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if depth >= 3 => {
                if let Ok(s) = t.decode() {
                    text.push_str(&s);
                }
            }
            // DBLP writes non-ASCII as character references (`&#252;`) and
            // `&amp;` for the rest; quick-xml hands each over as an event.
            Ok(Event::GeneralRef(r)) if depth >= 3 => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text.push(c);
                } else if let Ok(name) = r.decode() {
                    match name.as_ref() {
                        "amp" => text.push('&'),
                        "lt" => text.push('<'),
                        "gt" => text.push('>'),
                        "quot" => text.push('"'),
                        "apos" => text.push('\''),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(_)) => {
                if depth == 3 {
                    let value = text.trim().to_string();
                    if !value.is_empty() {
                        match field.as_str() {
                            "author" => record.authors.push(value),
                            "editor" => editors.push(value),
                            "title" => record.title = Some(value),
                            "booktitle" | "journal" => record.venue = Some(value),
                            "volume" => record.volume = Some(value),
                            "number" => record.number = Some(value),
                            "pages" => record.pages = Some(value),
                            "year" => record.year = value.parse().ok(),
                            "ee" => record.ee.push(value),
                            _ => {}
                        }
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if !found || record.key.is_empty() {
        return None;
    }
    // A proceedings volume has editors where an article has authors.
    if record.authors.is_empty() {
        record.authors = editors;
    }
    record.ty = match element.as_str() {
        "article" if informal || record.venue.as_deref() == Some("CoRR") => "article",
        "article" => "article-journal",
        "inproceedings" => "paper-conference",
        "proceedings" | "book" => "book",
        "incollection" => "chapter",
        "phdthesis" | "mastersthesis" => "thesis",
        "data" => "dataset",
        _ => "article",
    }
    .into();
    Some(record)
}

/// The canonical work of a record.
pub fn work(record: &Record, fetched_at: &str, query: Option<&Query>) -> Work {
    let informal = record.venue.as_deref() == Some("CoRR");
    let mut ids = BTreeMap::from([("dblp".to_string(), record.key.clone())]);
    let mut doi = record.doi.clone();
    let mut locations: Vec<Location> = Vec::new();
    for url in &record.ee {
        if doi.is_none() {
            doi = doi_of(url);
        }
        if let Some(a) = arxiv_of(url) {
            ids.entry("arxiv".into()).or_insert(a);
        }
        if locations
            .iter()
            .any(|l| l.url.as_deref() == Some(url.as_str()))
        {
            continue;
        }
        let submitted = informal || host_of(url) == "arxiv.org";
        locations.push(Location {
            source: SOURCE.into(),
            source_id: record.key.clone(),
            version: if submitted {
                Version::Submitted
            } else {
                Version::Published
            },
            url: Some(url.clone()),
            pdf: url
                .to_ascii_lowercase()
                .ends_with(".pdf")
                .then(|| url.clone()),
            license: None,
            oa_status: None,
            primary: locations.is_empty(),
        });
    }
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    Work {
        ty: if record.ty.is_empty() {
            "article".into()
        } else {
            record.ty.clone()
        },
        title: record
            .title
            .as_ref()
            .map(|t| t.trim_end_matches('.').trim().to_string()),
        author: record
            .authors
            .iter()
            .map(|a| author(&clean_name(a)))
            .collect(),
        issued: record.year.map(|y| Issued {
            date_parts: vec![vec![y]],
        }),
        container_title: record.venue.clone(),
        volume: record.volume.clone().filter(|v| !v.starts_with("abs/")),
        issue: record.number.clone(),
        page: record.pages.clone(),
        doi,
        url: Some(format!("https://dblp.org/rec/{}", record.key)),
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: record.key.clone(),
            query: query.and_then(|q| serde_json::to_string(q).ok()),
            sources: Vec::new(),
        }),
        ..Default::default()
    }
}
