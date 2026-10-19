//! An arXiv Atom feed → `bibliographic-work@1` (S-D4): the entry's CSL
//! fields, `ids.arxiv` (versionless) and `ids.doi` when the entry carries
//! one, one `submitted` location with the pdf url, `provenance`.
//!
//! What the feed has and the work kind has no field for is dropped, not
//! smuggled: `arxiv:comment`, `arxiv:journal_ref` (an unstructured string,
//! not a `container-title`), the categories and the affiliations.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use koine_sources::kind::{Author, Issued, Location, Provenance, Version};
use koine_sources::{Query, Work};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use regex::Regex;

use crate::SOURCE;

/// The modern id: `2101.00001`, `1706.03762v7`.
pub const NEW_ID: &str = r"^\d{4}\.\d{4,5}(v\d+)?$";
/// The legacy id: `astro-ph/0112088`, `math.GT/0309136v2`.
pub const OLD_ID: &str = r"^[a-z-]+(\.[A-Z]{2})?/\d{7}(v\d+)?$";

fn new_id() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(NEW_ID).expect("valid regex"))
}

fn old_id() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(OLD_ID).expect("valid regex"))
}

/// Whether `id` is an arXiv identifier, old or new, with or without `vN`.
pub fn is_arxiv_id(id: &str) -> bool {
    new_id().is_match(id) || old_id().is_match(id)
}

/// `astro-ph/0112088v1` → `astro-ph/0112088`.
pub fn versionless(id: &str) -> &str {
    match id.rfind('v') {
        Some(i) if i > 0 && id.len() > i + 1 && id[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            &id[..i]
        }
        _ => id,
    }
}

/// The arXiv id inside an entry `<id>` URL (`http://arxiv.org/abs/…`).
pub fn id_from_url(url: &str) -> Option<String> {
    let rest = url.split("/abs/").nth(1)?;
    is_arxiv_id(rest).then(|| rest.to_string())
}

/// One `<entry>`, as the feed gave it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub published: String,
    pub doi: String,
    pub authors: Vec<String>,
    pub abs_url: Option<String>,
    pub pdf_url: Option<String>,
}

/// A parsed feed: the entries and `opensearch:totalResults`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub total: Option<u64>,
    pub entries: Vec<Entry>,
}

fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key.as_bytes())
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn entity(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".into()),
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix('x') {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code).map(String::from)
        }
    }
}

/// Parse an Atom feed. `Err` is a document that is not XML.
pub fn feed(xml: &str) -> Result<Feed, String> {
    let mut reader = Reader::from_str(xml);
    let mut out = Feed::default();
    let mut current: Option<Entry> = None;
    let mut path: Vec<String> = Vec::new();
    let mut author_name = String::new();
    let mut total = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "entry" {
                    current = Some(Entry::default());
                }
                if name == "link" {
                    link(&e, &mut current);
                }
                path.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "link" {
                    link(&e, &mut current);
                }
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().map_err(|e| e.to_string())?;
                push_text(&path, &text, &mut current, &mut author_name, &mut total);
            }
            Ok(Event::GeneralRef(r)) => {
                let name = r.decode().map_err(|e| e.to_string())?;
                if let Some(text) = entity(&name) {
                    push_text(&path, &text, &mut current, &mut author_name, &mut total);
                }
            }
            Ok(Event::CData(c)) => {
                let text = String::from_utf8_lossy(&c).to_string();
                push_text(&path, &text, &mut current, &mut author_name, &mut total);
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "entry" => {
                        if let Some(entry) = current.take() {
                            out.entries.push(entry);
                        }
                    }
                    "author" => {
                        if let Some(entry) = current.as_mut() {
                            let n = author_name.trim().to_string();
                            if !n.is_empty() {
                                entry.authors.push(n);
                            }
                        }
                        author_name.clear();
                    }
                    _ => {}
                }
                path.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("arxiv atom: {e}")),
            _ => {}
        }
    }
    out.total = total.trim().parse().ok();
    Ok(out)
}

fn link(e: &BytesStart<'_>, current: &mut Option<Entry>) {
    let Some(entry) = current.as_mut() else {
        return;
    };
    let Some(href) = attr(e, "href") else {
        return;
    };
    let rel = attr(e, "rel").unwrap_or_else(|| "alternate".into());
    let ty = attr(e, "type").unwrap_or_default();
    if rel == "alternate" {
        entry.abs_url = Some(href);
    } else if ty == "application/pdf" || attr(e, "title").as_deref() == Some("pdf") {
        entry.pdf_url = Some(href);
    }
}

fn push_text(
    path: &[String],
    text: &str,
    current: &mut Option<Entry>,
    author_name: &mut String,
    total: &mut String,
) {
    let Some(tag) = path.last() else { return };
    if tag == "opensearch:totalResults" {
        total.push_str(text);
        return;
    }
    let Some(entry) = current.as_mut() else {
        return;
    };
    let direct = path.len() >= 2 && path[path.len() - 2] == "entry";
    match tag.as_str() {
        "id" if direct => entry.id.push_str(text),
        "title" if direct => entry.title.push_str(text),
        "summary" if direct => entry.summary.push_str(text),
        "published" if direct => entry.published.push_str(text),
        "arxiv:doi" => entry.doi.push_str(text),
        "name" => author_name.push_str(text),
        _ => {}
    }
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Tom Abel` → family `Abel`, given `Tom`; one token is a family name.
fn author(name: &str) -> Author {
    let parts: Vec<&str> = name.split_whitespace().collect();
    match parts.split_last() {
        Some((family, given)) if !given.is_empty() => Author {
            family: Some((*family).to_string()),
            given: Some(given.join(" ")),
            orcid: None,
        },
        _ => Author {
            family: Some(name.trim().to_string()),
            ..Default::default()
        },
    }
}

/// `2001-12-04T19:38:08Z` → `[[2001, 12, 4]]`.
fn issued(published: &str) -> Option<Issued> {
    let date = published.split('T').next()?;
    let parts: Vec<i32> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    (parts.len() == 3).then(|| Issued {
        date_parts: vec![parts],
    })
}

/// The error entry arXiv answers a bad request with, as `Some(message)`.
pub fn error_of(entry: &Entry) -> Option<String> {
    entry
        .id
        .contains("/api/errors")
        .then(|| clean(&entry.summary))
}

/// One entry → a work; `None` when it names no arXiv id.
pub fn work(entry: &Entry, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let versioned = id_from_url(entry.id.trim())?;
    let id = versionless(&versioned).to_string();
    let mut ids = BTreeMap::from([("arxiv".to_string(), id.clone())]);
    let doi = Some(entry.doi.trim().to_string()).filter(|d| !d.is_empty());
    if let Some(d) = &doi {
        ids.insert("doi".into(), d.clone());
    }
    let abs = entry
        .abs_url
        .clone()
        .unwrap_or_else(|| format!("https://arxiv.org/abs/{versioned}"));
    let summary = clean(&entry.summary);
    Some(Work {
        ty: "article".into(),
        title: Some(clean(&entry.title)).filter(|t| !t.is_empty()),
        author: entry.authors.iter().map(|a| author(a)).collect(),
        issued: issued(&entry.published),
        abstract_: Some(summary).filter(|s| !s.is_empty()),
        doi,
        url: Some(abs.clone()),
        ids,
        locations: vec![Location {
            source: SOURCE.into(),
            source_id: versioned,
            version: Version::Submitted,
            url: Some(abs),
            pdf: entry.pdf_url.clone(),
            license: None,
            oa_status: Some("green".into()),
            primary: true,
        }],
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id: id,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
        }),
        ..Default::default()
    })
}
