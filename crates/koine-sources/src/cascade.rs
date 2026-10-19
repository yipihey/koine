//! The resolution cascade (S-D6), as a pure function over two trait objects.
//!
//! `resolveStructuredCitation` in imbib does I/O at every step; here the
//! steps are the same but the lookups are [`LocalLookup`] and
//! [`RemoteSearch`], so the cascade has no I/O of its own and a stub can
//! drive every branch in a unit test. The three steps:
//!
//! 1. **Identifier.** A DOI, arXiv id or bibcode in the input, or scanned
//!    from its BibTeX or free text, is looked up locally then remotely. A
//!    hit is [`Via::Identifier`] with score 1.
//! 2. **Local store.** The library's own text search over the title (or the
//!    BibTeX fragment), scored; the top hit at or above
//!    [`LOCAL_THRESHOLD`] is [`Via::Local`].
//! 3. **Search.** The remote structured search, scored, ranked, and put to
//!    the acceptance rules; [`Via::Search`], with the rule that fired or
//!    none. **Nothing auto-merges below threshold**: the candidates come
//!    back with their scores and evidence for a person or agent to choose.
//!
//! Swift's fourth step (an all-sources free-text fallback) is the
//! federation's job (S-D7) and is not a step here: a [`RemoteSearch`] may
//! already fan out.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kind::Work;
use crate::score::{Acceptance, CitationInput, Evidence, Rule, Scorer};

/// The local-text step accepts its top hit at or above this.
pub const LOCAL_THRESHOLD: f64 = 0.55;
/// How many local text hits are scored.
pub const LOCAL_LIMIT: usize = 5;
/// How many remote hits are asked for, and the most candidates returned.
pub const SEARCH_LIMIT: usize = 10;

/// One canonical identifier, `kind` from [`crate::kind::IDENTIFIER_KINDS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Identifier {
    pub kind: String,
    pub value: String,
}

impl Identifier {
    pub fn new(kind: &str, value: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            value: value.into(),
        }
    }
}

/// The library's own rows.
pub trait LocalLookup {
    /// A row carrying this identifier.
    fn by_identifier(&self, id: &Identifier) -> Option<Work>;
    /// Text search over title and authors, at most `limit` rows.
    fn search_text(&self, query: &str, limit: usize) -> Vec<Work>;
}

/// The online sources.
pub trait RemoteSearch {
    /// Fetch one record by identifier.
    fn fetch(&self, id: &Identifier) -> Option<Work>;
    /// A structured search for the citation, at most `limit` rows; the
    /// citation's LaTeX is already decoded.
    fn search(&self, citation: &CitationInput, limit: usize) -> Vec<Work>;
}

/// Which step answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Via {
    Identifier,
    Local,
    Search,
    NotFound,
}

/// A candidate with its score and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
    pub work: Work,
    pub score: f64,
    pub evidence: Evidence,
}

/// The cascade's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    pub via: Via,
    /// Index into `candidates` of the one accepted, if any step accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<usize>,
    /// The search-step rule that fired, when `via` is `search`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
    /// Best first; the search step returns only those at or above
    /// [`crate::score::KEEP_THRESHOLD`], at most [`SEARCH_LIMIT`].
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// The identifier step 1 looked up, if the input had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<Identifier>,
}

/// Run the cascade.
pub fn resolve(
    scorer: &Scorer,
    citation: &CitationInput,
    local: &dyn LocalLookup,
    remote: &dyn RemoteSearch,
) -> Resolution {
    let decoded = scorer.decode(citation);
    let identifier = identifier_of(scorer, &decoded);

    // Step 1: identifier.
    if let Some(id) = &identifier {
        if let Some(work) = local.by_identifier(id).or_else(|| remote.fetch(id)) {
            return Resolution {
                via: Via::Identifier,
                accepted: Some(0),
                rule: None,
                candidates: vec![Candidate {
                    work,
                    score: 1.0,
                    evidence: Evidence {
                        identifier: Some(1.0),
                        ..Evidence::default()
                    },
                }],
                identifier,
            };
        }
    }

    // Step 2: local text.
    let local_query = decoded
        .title
        .as_deref()
        .filter(|t| t.chars().count() >= 10)
        .map(str::to_string)
        .or_else(|| {
            decoded
                .raw_bibtex
                .as_deref()
                .filter(|b| !b.is_empty())
                .map(|b| b.chars().take(120).collect())
        });
    if let Some(query) = local_query {
        let hits = local.search_text(&query, LOCAL_LIMIT);
        let candidates = score_and_rank(scorer, &decoded, hits);
        if candidates
            .first()
            .is_some_and(|c| c.score >= LOCAL_THRESHOLD)
        {
            return Resolution {
                via: Via::Local,
                accepted: Some(0),
                rule: None,
                candidates,
                identifier,
            };
        }
    }

    // Step 3: search, scored and put to the rules.
    let hits = remote.search(&decoded, SEARCH_LIMIT);
    let raw_hit_count = hits.len();
    let scored: Vec<_> = hits
        .iter()
        .map(|w| scorer.score_decoded(&decoded, w))
        .collect();
    let Acceptance {
        accepted,
        rule,
        kept,
    } = scorer.accept(&scored, raw_hit_count);
    let candidates: Vec<Candidate> = kept
        .iter()
        .take(SEARCH_LIMIT)
        .map(|&i| Candidate {
            work: hits[i].clone(),
            score: scored[i].score,
            evidence: scored[i].evidence,
        })
        .collect();
    if candidates.is_empty() {
        return Resolution {
            via: Via::NotFound,
            accepted: None,
            rule: None,
            candidates,
            identifier,
        };
    }
    Resolution {
        via: Via::Search,
        // An accepted top is always kept (0.50 ≥ 0.30), and is first.
        accepted: accepted.map(|_| 0),
        rule,
        candidates,
        identifier,
    }
}

fn score_and_rank(scorer: &Scorer, decoded: &CitationInput, hits: Vec<Work>) -> Vec<Candidate> {
    let scored: Vec<_> = hits
        .iter()
        .map(|w| scorer.score_decoded(decoded, w))
        .collect();
    scorer
        .rank(&scored)
        .into_iter()
        .map(|i| Candidate {
            work: hits[i].clone(),
            score: scored[i].score,
            evidence: scored[i].evidence,
        })
        .collect()
}

/// The identifier step 1 uses: the input's `doi`, `arxiv` or `bibcode`
/// field (in that order), else one scanned from `raw_bibtex` (LaTeX-decoded,
/// as Swift does), else from `free_text`.
pub fn identifier_of(scorer: &Scorer, citation: &CitationInput) -> Option<Identifier> {
    let field = |kind: &str, v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Identifier::new(kind, s))
    };
    field("doi", &citation.doi)
        .or_else(|| field("arxiv", &citation.arxiv))
        .or_else(|| field("bibcode", &citation.bibcode))
        .or_else(|| {
            let bibtex = citation.raw_bibtex.as_deref()?;
            scan_identifier(&crate::latex::decode_latex_with(
                bibtex,
                scorer.quirks.backslash_dotless_i_decode,
            ))
        })
        .or_else(|| scan_identifier(citation.free_text.as_deref()?))
}

static DOI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(10\.\d{4,9}/[^\s"'<>,;{}]+)"#).expect("static regex"));
static ARXIV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:arxiv:|arxiv\.org/abs/|\beprint\s*=\s*[{\x22]?\s*)(\d{4}\.\d{4,5}(?:v\d+)?|[a-z][a-z.-]+/\d{7}(?:v\d+)?)",
    )
    .expect("static regex")
});
static BIBCODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\bbibcode\s*=\s*[{\x22]?\s*|adsabs\.harvard\.edu/abs/)(\d{4}[A-Za-z0-9.&]{14}[A-Za-z.])").expect("static regex")
});

/// A minimal scan of free text or BibTeX for one identifier: DOI first,
/// then arXiv, then bibcode. Small on purpose (`im-identifiers` lives in the
/// other repository); a caller with a real parser passes the fields.
pub fn scan_identifier(text: &str) -> Option<Identifier> {
    if text.is_empty() {
        return None;
    }
    let clean = |s: &str| s.trim_end_matches(['.', ')', '}', '"']).to_string();
    if let Some(m) = DOI_RE.captures(text) {
        return Some(Identifier::new("doi", clean(&m[1])));
    }
    if let Some(m) = ARXIV_RE.captures(text) {
        return Some(Identifier::new("arxiv", clean(&m[1])));
    }
    if let Some(m) = BIBCODE_RE.captures(text) {
        return Some(Identifier::new("bibcode", m[1].to_string()));
    }
    None
}

// ── Source priority and dedup ───────────────────────────────────────────────

/// The one source-priority table (lower wins a dedup tie).
///
/// Decided from the two that disagreed: `imbib-core`'s `SOURCE_PRIORITY`
/// (crossref 10, pubmed 20, ads 30, semanticscholar 40, openalex 50, arxiv
/// 60, dblp 70, unknown 100) is the base, and the two sources only Swift's
/// `deduplicationPriority` knew are slotted where Swift had them relative to
/// ADS: `scix` at 31 (Swift: ads 30, scix 31) and `wos` at 25, ahead of ADS
/// as a curated index. Swift's own numbers for the shared sources (crossref
/// 40, pubmed 50) are dropped: the Rust table already ranked the publisher
/// source first, which is the right order for a DOI-bearing merge.
/// `semanticscholar` is accepted as a spelling of `s2`.
pub fn source_priority(source: &str) -> u32 {
    match source.to_ascii_lowercase().as_str() {
        "crossref" => 10,
        "pubmed" => 20,
        "wos" => 25,
        "ads" => 30,
        "scix" => 31,
        "s2" | "semanticscholar" => 40,
        "openalex" => 50,
        "arxiv" => 60,
        "dblp" => 70,
        _ => 100,
    }
}

/// The source a work came from: its provenance, else its primary location,
/// else its first location, else `unknown`.
pub fn source_of(work: &Work) -> &str {
    if let Some(p) = &work.provenance {
        return &p.source;
    }
    work.locations
        .iter()
        .find(|l| l.primary)
        .or(work.locations.first())
        .map_or("unknown", |l| l.source.as_str())
}

/// Collapse works that share an identifier (DOI, arXiv, bibcode: the first
/// non-empty one, lowercased, as Swift's `dedupByIdentifier`), keeping the
/// one from the highest-priority source; among equals, the first seen.
/// Output order is the first appearance of each key. A work with no
/// identifier is keyed by `source:title`.
pub fn dedup_by_identifier(works: Vec<Work>) -> Vec<Work> {
    let key = |w: &Work| -> String {
        let doi = w.doi.as_deref().or(w.ids.get("doi").map(String::as_str));
        [
            doi,
            w.ids.get("arxiv").map(String::as_str),
            w.ids.get("bibcode").map(String::as_str),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .map(str::to_lowercase)
        .unwrap_or_else(|| format!("{}:{}", source_of(w), w.title.as_deref().unwrap_or("")))
    };
    let mut keys: Vec<String> = Vec::new();
    let mut best: Vec<Work> = Vec::new();
    let mut seen = BTreeSet::new();
    for w in works {
        let k = key(&w);
        if seen.insert(k.clone()) {
            keys.push(k);
            best.push(w);
        } else if let Some(i) = keys.iter().position(|x| *x == k) {
            if source_priority(source_of(&w)) < source_priority(source_of(&best[i])) {
                best[i] = w;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_identifiers() {
        assert_eq!(
            scan_identifier("see https://doi.org/10.1086/164143."),
            Some(Identifier::new("doi", "10.1086/164143"))
        );
        assert_eq!(
            scan_identifier("arXiv:1807.06209v2"),
            Some(Identifier::new("arxiv", "1807.06209v2"))
        );
        assert_eq!(
            scan_identifier("eprint = {astro-ph/0505010}, archivePrefix = {arXiv}"),
            Some(Identifier::new("arxiv", "astro-ph/0505010"))
        );
        assert_eq!(
            scan_identifier("bibcode = {1986ApJ...304...15B}"),
            Some(Identifier::new("bibcode", "1986ApJ...304...15B"))
        );
        assert_eq!(scan_identifier("Navarro, Frenk & White 1997"), None);
    }

    use crate::kind::{Author, Issued, Location, Version};
    use std::collections::BTreeMap;

    fn work(
        title: &str,
        family: &str,
        year: i32,
        venue: &str,
        doi: Option<&str>,
        source: &str,
    ) -> Work {
        let mut ids = BTreeMap::new();
        if let Some(d) = doi {
            ids.insert("doi".to_string(), d.to_string());
        }
        Work {
            ty: "article-journal".into(),
            title: Some(title.into()),
            author: vec![Author {
                family: Some(family.into()),
                given: Some("J.".into()),
                orcid: None,
            }],
            issued: Some(Issued {
                date_parts: vec![vec![year]],
            }),
            container_title: Some(venue.into()),
            doi: doi.map(str::to_string),
            ids,
            locations: vec![Location {
                source: source.into(),
                source_id: "x".into(),
                version: Version::Published,
                url: None,
                pdf: None,
                license: None,
                oa_status: None,
                primary: true,
            }],
            ..Work::default()
        }
    }

    fn nfw() -> Work {
        work(
            "A Universal Density Profile from Hierarchical Clustering",
            "Navarro",
            1997,
            "The Astrophysical Journal",
            Some("10.1086/304888"),
            "ads",
        )
    }

    fn nfw_citation() -> CitationInput {
        CitationInput {
            authors: vec!["Navarro, J. F.".into()],
            title: Some("A Universal Density Profile from Hierarchical Clustering".into()),
            year: Some(1997),
            journal: Some("ApJ".into()),
            ..CitationInput::default()
        }
    }

    #[derive(Default)]
    struct Stub {
        by_id: Vec<(Identifier, Work)>,
        local_text: Vec<Work>,
        remote_fetch: Vec<(Identifier, Work)>,
        remote_search: Vec<Work>,
    }

    impl LocalLookup for Stub {
        fn by_identifier(&self, id: &Identifier) -> Option<Work> {
            self.by_id
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, w)| w.clone())
        }
        fn search_text(&self, _query: &str, limit: usize) -> Vec<Work> {
            self.local_text.iter().take(limit).cloned().collect()
        }
    }

    impl RemoteSearch for Stub {
        fn fetch(&self, id: &Identifier) -> Option<Work> {
            self.remote_fetch
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, w)| w.clone())
        }
        fn search(&self, _citation: &CitationInput, limit: usize) -> Vec<Work> {
            self.remote_search.iter().take(limit).cloned().collect()
        }
    }

    #[test]
    fn via_identifier_from_a_field_hits_local_first() {
        let doi = Identifier::new("doi", "10.1086/304888");
        let stub = Stub {
            by_id: vec![(doi.clone(), nfw())],
            remote_search: vec![work("Decoy", "Navarro", 1997, "ApJ", None, "ads")],
            ..Stub::default()
        };
        let citation = CitationInput {
            doi: Some("10.1086/304888".into()),
            ..CitationInput::default()
        };
        let r = resolve(&Scorer::legacy(), &citation, &stub, &stub);
        assert_eq!(r.via, Via::Identifier);
        assert_eq!(r.accepted, Some(0));
        assert_eq!(r.identifier, Some(doi));
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].score, 1.0);
        assert_eq!(r.candidates[0].evidence.identifier, Some(1.0));
        assert_eq!(
            r.candidates[0].work.title.as_deref(),
            Some(nfw().title.as_deref().unwrap())
        );
    }

    #[test]
    fn via_identifier_scanned_from_bibtex_falls_to_remote_fetch() {
        let arxiv = Identifier::new("arxiv", "astro-ph/9611107");
        let stub = Stub {
            remote_fetch: vec![(arxiv.clone(), nfw())],
            ..Stub::default()
        };
        let citation = CitationInput {
            raw_bibtex: Some(
                "@article{nfw, eprint = {astro-ph/9611107}, archivePrefix = {arXiv}}".into(),
            ),
            ..CitationInput::default()
        };
        let r = resolve(&Scorer::legacy(), &citation, &stub, &stub);
        assert_eq!(r.via, Via::Identifier);
        assert_eq!(r.identifier, Some(arxiv));
    }

    #[test]
    fn via_local_when_the_top_text_hit_clears_0_55() {
        let stub = Stub {
            local_text: vec![
                work(
                    "Density profiles of something else",
                    "Other",
                    2001,
                    "MNRAS",
                    None,
                    "local",
                ),
                nfw(),
            ],
            ..Stub::default()
        };
        let r = resolve(&Scorer::legacy(), &nfw_citation(), &stub, &stub);
        assert_eq!(r.via, Via::Local);
        assert_eq!(r.accepted, Some(0));
        assert_eq!(r.rule, None);
        // Ranked: the real paper first, the weak hit second, both returned.
        assert_eq!(r.candidates.len(), 2);
        assert!(r.candidates[0].score >= LOCAL_THRESHOLD);
        assert!(r.candidates[1].score < LOCAL_THRESHOLD);
        assert_eq!(r.candidates[0].evidence.first_author, Some(0.25));
    }

    #[test]
    fn via_search_accepts_by_rule_and_names_it() {
        let stub = Stub {
            local_text: vec![work(
                "Density profiles of something else",
                "Other",
                2001,
                "MNRAS",
                None,
                "local",
            )],
            remote_search: vec![
                work(
                    "Something else altogether",
                    "Smith",
                    1980,
                    "Nature",
                    None,
                    "ads",
                ),
                nfw(),
            ],
            ..Stub::default()
        };
        let r = resolve(&Scorer::legacy(), &nfw_citation(), &stub, &stub);
        assert_eq!(r.via, Via::Search);
        assert_eq!(r.rule, Some(Rule::HighConfidence));
        assert_eq!(r.accepted, Some(0));
        // Only the kept candidate (>= 0.30) comes back; the 0.00 decoy does not.
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].score, 1.0);
        assert_eq!(r.candidates[0].evidence.sum().min(1.0), 1.0);
    }

    #[test]
    fn via_search_below_threshold_returns_candidates_unaccepted() {
        let stub = Stub {
            remote_search: vec![
                // Same author and year, nothing else: 0.25 + 0.20 + 0.15 = 0.60.
                work(
                    "Halo shapes",
                    "Navarro",
                    1997,
                    "Icarus",
                    Some("10.1/a"),
                    "ads",
                ),
                // Year only, plus id: 0.35.
                work(
                    "Unrelated",
                    "Zed",
                    1997,
                    "Icarus",
                    Some("10.1/b"),
                    "crossref",
                ),
            ],
            ..Stub::default()
        };
        let citation = CitationInput {
            authors: vec!["Navarro, J. F.".into()],
            title: Some("Halo profiles".into()),
            year: Some(1997),
            ..CitationInput::default()
        };
        let r = resolve(&Scorer::legacy(), &citation, &stub, &stub);
        assert_eq!(r.via, Via::Search);
        assert_eq!(r.accepted, None, "nothing auto-merges below threshold");
        assert_eq!(r.rule, None);
        assert_eq!(r.candidates.len(), 2);
        assert!(r.candidates[0].score > r.candidates[1].score);
    }

    #[test]
    fn via_not_found_when_nothing_is_kept() {
        let stub = Stub {
            remote_search: vec![work(
                "Unrelated",
                "Zed",
                1950,
                "Icarus",
                Some("10.1/b"),
                "crossref",
            )],
            ..Stub::default()
        };
        let r = resolve(&Scorer::legacy(), &nfw_citation(), &stub, &stub);
        assert_eq!(r.via, Via::NotFound);
        assert!(r.candidates.is_empty());
        assert_eq!(r.accepted, None);
    }

    #[test]
    fn dedup_keeps_the_higher_priority_source_in_first_seen_order() {
        let a_arxiv = work("A", "X", 2020, "v", Some("10.1/a"), "arxiv");
        let a_crossref = work("A", "X", 2020, "v", Some("10.1/A"), "crossref");
        let b = work("B", "Y", 2020, "v", None, "ads");
        let out = dedup_by_identifier(vec![a_arxiv, b.clone(), a_crossref]);
        assert_eq!(out.len(), 2);
        assert_eq!(
            source_of(&out[0]),
            "crossref",
            "crossref replaces arxiv in place"
        );
        assert_eq!(out[1], b);
    }

    #[test]
    fn priority_table() {
        assert_eq!(source_priority("crossref"), 10);
        assert_eq!(source_priority("wos"), 25);
        assert_eq!(source_priority("ADS"), 30);
        assert_eq!(source_priority("scix"), 31);
        assert_eq!(source_priority("s2"), source_priority("semanticscholar"));
        assert_eq!(source_priority("nowhere"), 100);
        assert!(source_priority("crossref") < source_priority("arxiv"));
    }
}
