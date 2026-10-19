//! A Semantic Scholar paper object → `bibliographic-work@1` (S-D4).
//!
//! Choices, where S2 and the canonical record disagree:
//!
//! - **`ids`**: `doi` (`externalIds.DOI`), `arxiv` (`ArXiv`), `pmid`
//!   (`PubMed`), `pmcid` (`PubMedCentral`, given the `PMC` prefix S2 omits),
//!   `s2` (`paperId`), `mag` (`MAG`), `dblp` (`DBLP`). The numeric `corpusId`
//!   is *not* carried: `corpusid` is not a canonical id kind and
//!   `Work::validate` refuses an `ids` key outside the list. A `corpusid:N`
//!   still resolves (`fetch`), it just does not come back on a row.
//! - **Authors**: S2 gives display names only. The family/given split is the
//!   OpenAlex heuristic, a little better: the last token is the family name,
//!   joined to a directly preceding particle (`van`, `de`, `von`, …) or
//!   followed suffix (`Jr.`, `III`); a name with a comma is read as
//!   `Family, Given`; one token is all family. "N. Parmar" → Parmar / N.
//! - **Container**: `journal.name`, except S2's `ArXiv` placeholder, then
//!   `venue`, then `publicationVenue.name`. `journal.volume` of the form
//!   `abs/…` (arXiv's) is dropped.
//! - **Type**: `publicationTypes` → CSL, `Conference` winning over
//!   `JournalArticle` (S2 tags NeurIPS/ACL papers with both); none → `article`.
//! - **Locations** (the version rule): the DOI is the `published` location
//!   (`primary`); `openAccessPdf.url`, when not empty, is another: on a
//!   preprint/repository host (arxiv, biorxiv, medrxiv, chemrxiv, ssrn, hal,
//!   osf, zenodo, researchgate, core.ac.uk, europepmc) it is `submitted`;
//!   one that is the DOI's own landing page (S2 spells it in lower case)
//!   adds its licence and `oa_status` to the DOI location instead of a second;
//!   `status: GREEN` elsewhere is `accepted`; GOLD, HYBRID, BRONZE and DIAMOND
//!   are `published`. A paper with an arXiv id and no location pointing at
//!   arxiv.org gets an arxiv `submitted` location (abs + pdf). `isOpenAccess:
//!   false` with an empty url yields no OA location.
//! - **Metrics**: `metrics.cited_by` is `citationCount`.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Counted, Issued, Location, Metrics, Provenance, Version};
use koine_sources::{Query, Work};
use serde_json::Value;

use crate::SOURCE;

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
}

const PARTICLES: &[&str] = &[
    "van", "von", "de", "der", "den", "del", "della", "di", "da", "dos", "du", "la", "le", "bin",
    "ibn", "al", "el", "st.", "ter", "ten",
];
const SUFFIXES: &[&str] = &["jr.", "jr", "sr.", "sr", "ii", "iii", "iv"];

/// A display name → family/given (see the module docs).
pub fn author(name: &str) -> Author {
    let name = name.trim();
    if let Some((family, given)) = name.split_once(',') {
        let given = given.trim();
        return Author {
            family: Some(family.trim().to_string()),
            given: (!given.is_empty()).then(|| given.to_string()),
            orcid: None,
        };
    }
    let parts: Vec<&str> = name.split_whitespace().collect();
    if parts.len() < 2 {
        return Author {
            family: (!name.is_empty()).then(|| name.to_string()),
            ..Default::default()
        };
    }
    let mut end = parts.len();
    // A suffix stays with the family name before it.
    if end >= 3 && SUFFIXES.contains(&parts[end - 1].to_ascii_lowercase().as_str()) {
        end -= 1;
    }
    let mut start = end - 1;
    while start > 1 && PARTICLES.contains(&parts[start - 1].to_ascii_lowercase().as_str()) {
        start -= 1;
    }
    Author {
        family: Some(parts[start..].join(" ")),
        given: Some(parts[..start].join(" ")),
        orcid: None,
    }
}

fn csl_type(types: &[String]) -> &'static str {
    let has = |t: &str| types.iter().any(|x| x == t);
    if has("Conference") {
        "paper-conference"
    } else if has("JournalArticle") || has("Review") || has("CaseReport") || has("MetaAnalysis") {
        "article-journal"
    } else if has("Book") {
        "book"
    } else if has("BookSection") {
        "chapter"
    } else if has("Dataset") {
        "dataset"
    } else {
        "article"
    }
}

const REPOSITORIES: &[&str] = &[
    "arxiv.org",
    "biorxiv.org",
    "medrxiv.org",
    "chemrxiv.org",
    "ssrn.com",
    "hal.science",
    "hal.archives-ouvertes.fr",
    "osf.io",
    "zenodo.org",
    "researchgate.net",
    "core.ac.uk",
    "europepmc.org",
];

fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
}

/// The DRIVER version of an `openAccessPdf` (the module docs' rule).
pub fn oa_version(url: &str, status: Option<&str>) -> Version {
    let host = host_of(url).to_ascii_lowercase();
    if REPOSITORIES
        .iter()
        .any(|r| host == *r || host.ends_with(&format!(".{r}")))
    {
        return Version::Submitted;
    }
    match status {
        Some("GREEN") => Version::Accepted,
        _ => Version::Published,
    }
}

fn issued(p: &Value) -> Option<Issued> {
    if let Some(date) = s(p.get("publicationDate")) {
        let parts: Result<Vec<i32>, _> = date.split('-').map(|x| x.parse::<i32>()).collect();
        if let Ok(parts) = parts {
            if !parts.is_empty() {
                return Some(Issued {
                    date_parts: vec![parts],
                });
            }
        }
    }
    p.get("year").and_then(Value::as_i64).map(|y| Issued {
        date_parts: vec![vec![y as i32]],
    })
}

/// Parse one paper. `None` when it has no `paperId` and no DOI to name it by.
pub fn work(p: &Value, fetched_at: &str, query: Option<&Query>) -> Option<Work> {
    let ext = p.get("externalIds");
    let ext_s = |k: &str| ext.and_then(|e| s(e.get(k)));
    let paper_id = s(p.get("paperId"));
    let doi = ext_s("DOI");
    let source_id = paper_id.clone().or_else(|| doi.clone())?;
    title_guard(p)?;

    let mut ids = BTreeMap::new();
    if let Some(d) = &doi {
        ids.insert("doi".to_string(), d.clone());
    }
    if let Some(a) = ext_s("ArXiv") {
        ids.insert("arxiv".into(), a);
    }
    if let Some(a) = ext_s("PubMed") {
        ids.insert("pmid".into(), a);
    }
    if let Some(a) = ext_s("PubMedCentral") {
        ids.insert(
            "pmcid".into(),
            if a.starts_with("PMC") {
                a
            } else {
                format!("PMC{a}")
            },
        );
    }
    if let Some(id) = &paper_id {
        ids.insert("s2".into(), id.clone());
    }
    if let Some(a) = ext_s("MAG") {
        ids.insert("mag".into(), a);
    }
    if let Some(a) = ext_s("DBLP") {
        ids.insert("dblp".into(), a);
    }

    let journal = p.get("journal");
    let container = s(journal.and_then(|j| j.get("name")))
        .filter(|n| !n.eq_ignore_ascii_case("arxiv"))
        .or_else(|| s(p.get("venue")))
        .or_else(|| s(p.get("publicationVenue").and_then(|v| v.get("name"))));
    let volume = s(journal.and_then(|j| j.get("volume"))).filter(|v| !v.starts_with("abs/"));

    let types: Vec<String> = p
        .get("publicationTypes")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let source_url = paper_id
        .as_ref()
        .map(|id| format!("https://www.semanticscholar.org/paper/{id}"));
    let mut locations: Vec<Location> = Vec::new();
    let location =
        |version, url: String, pdf: Option<String>, license, oa_status, primary| Location {
            source: SOURCE.into(),
            source_id: source_id.clone(),
            version,
            url: Some(url),
            pdf,
            license,
            oa_status,
            primary,
        };
    if let Some(d) = &doi {
        locations.push(location(
            Version::Published,
            format!("https://doi.org/{d}"),
            None,
            None,
            None,
            true,
        ));
    }
    if let Some(oa) = p.get("openAccessPdf") {
        if let Some(url) = s(oa.get("url")) {
            let status = s(oa.get("status"));
            let license = s(oa.get("license"));
            let same_as_doi = locations
                .first()
                .and_then(|l| l.url.as_deref())
                .is_some_and(|u| u.eq_ignore_ascii_case(&url));
            if same_as_doi {
                // The OA copy is the DOI's own landing page: one location.
                locations[0].license = license;
                locations[0].oa_status = status.map(|st| st.to_ascii_lowercase());
            } else {
                let version = oa_version(&url, status.as_deref());
                let is_pdf = url.to_ascii_lowercase().contains("pdf");
                locations.push(location(
                    version,
                    url.clone(),
                    is_pdf.then(|| url.clone()),
                    license,
                    status.map(|st| st.to_ascii_lowercase()),
                    doi.is_none(),
                ));
            }
        }
    }
    if let Some(a) = ids.get("arxiv") {
        let covered = locations
            .iter()
            .any(|l| l.url.as_deref().map(host_of) == Some("arxiv.org"));
        if !covered {
            locations.push(location(
                Version::Submitted,
                format!("https://arxiv.org/abs/{a}"),
                Some(format!("https://arxiv.org/pdf/{a}")),
                None,
                Some("green".into()),
                doi.is_none() && locations.is_empty(),
            ));
        }
    }

    Some(Work {
        ty: csl_type(&types).to_string(),
        title: s(p.get("title")),
        author: p
            .get("authors")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|a| s(a.get("name")))
                    .map(|n| author(&n))
                    .collect()
            })
            .unwrap_or_default(),
        issued: issued(p),
        container_title: container,
        volume,
        issue: s(journal.and_then(|j| j.get("issue"))),
        page: s(journal.and_then(|j| j.get("pages"))),
        abstract_: s(p.get("abstract")),
        language: None,
        doi,
        url: source_url,
        ids,
        locations,
        provenance: Some(Provenance {
            fetched_at: fetched_at.into(),
            source: SOURCE.into(),
            source_id,
            query: query.and_then(|q| serde_json::to_string(q).ok()),
            sources: Vec::new(),
        }),
        metrics: p
            .get("citationCount")
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

/// An unresolved reference S2 lists with neither a title nor an id carries
/// nothing to show.
fn title_guard(p: &Value) -> Option<()> {
    (s(p.get("title")).is_some() || s(p.get("paperId")).is_some()).then_some(())
}
