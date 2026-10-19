//! The citation-resolution scorer and its acceptance rules (S-D6).
//!
//! A port of imbib's `AutomationService.scoreCandidate` and the auto-accept
//! closure of `resolveStructuredCitation`, pinned bit-exactly by
//! `fixtures/citation-scorer-goldens.json` (98 cases recorded from the Swift
//! scorer, work package S2a). The goldens pin *behaviour*, not correctness:
//! `fixtures/citation-scorer-NOTES.md` lists sixteen recorded decisions that
//! look wrong. Each one that is arithmetic lives here behind a named switch
//! in [`Quirks`], so [`Quirks::legacy`] reproduces Swift and
//! [`Quirks::corrected`] is an explicit, testable departure from it — never a
//! silent fix.
//!
//! The terms are added in Swift's order (author, year, venue, title,
//! identifier) in f64 and compared with `>=`, because the thresholds sit on
//! float sums (`0.85` is recorded as `0.8500000000000001`; NOTES item 15).
//! Strings are NFC-normalised before comparison because Swift's `==` is
//! canonical-equivalence aware (NOTES item 8).
//!
//! Every answer carries its [`Evidence`]: the terms that fired and their
//! values, so a caller (and the person behind it) can see why a score is
//! what it is. `min(1, evidence.sum()) == score`, exactly.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use crate::kind::{Author, Work};
use crate::latex;

// ── Weights and thresholds (the constants S-D6 names) ───────────────────────

/// First-author surname equal.
pub const W_AUTHOR_EXACT: f64 = 0.25;
/// First-author surname partial (candidate's contains the query's).
pub const W_AUTHOR_PARTIAL: f64 = 0.15;
/// Year equal.
pub const W_YEAR_EXACT: f64 = 0.20;
/// Year within one, when not equal.
pub const W_YEAR_ADJACENT: f64 = 0.08;
/// Venue match (substring either way, or the bibstem table).
pub const W_VENUE: f64 = 0.20;
/// Title overlap of 5 or more words (legacy), or 75 % (corrected).
pub const W_TITLE_HIGH: f64 = 0.20;
/// Title overlap of 3–4 words (legacy), or 50 % (corrected).
pub const W_TITLE_MID: f64 = 0.12;
/// Title overlap of 1–2 words (legacy), or any (corrected).
pub const W_TITLE_LOW: f64 = 0.05;
/// The candidate carries a usable identifier.
pub const W_IDENTIFIER: f64 = 0.15;

/// "high-confidence": top at least this …
pub const ACCEPT_HIGH_TOP: f64 = 0.85;
/// … with at least this margin over the runner-up.
pub const ACCEPT_HIGH_MARGIN: f64 = 0.15;
/// "single-hit": the only hit, at least this.
pub const ACCEPT_SINGLE_TOP: f64 = 0.50;
/// "margin": top at least this …
pub const ACCEPT_MARGIN_TOP: f64 = 0.70;
/// … with at least this margin over the runner-up.
pub const ACCEPT_MARGIN_MARGIN: f64 = 0.25;
/// Candidates below this are not returned at all.
pub const KEEP_THRESHOLD: f64 = 0.30;

// ── Input ───────────────────────────────────────────────────────────────────

/// A structured citation, as a caller has it: the fields of imbib's
/// `CitationInput` that the scorer and cascade read. Authors are strings in
/// whatever form the caller found them (`"Bardeen, J. M."`, `"J. M.
/// Bardeen"`, `"Planck Collaboration"`); LaTeX in `authors`, `title` and
/// `journal` is decoded by the scorer, never by the caller.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CitationInput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arxiv: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bibcode: Option<String>,
    /// A BibTeX fragment the caller has; the cascade scans it for an
    /// identifier before searching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_bibtex: Option<String>,
    /// The reference as free text; scanned for an identifier after
    /// `raw_bibtex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
}

// ── Quirks ──────────────────────────────────────────────────────────────────

/// The recorded defects of the Swift scorer that affect arithmetic, each as a
/// switch. `true` is the Swift behaviour. The number after each is the item
/// in `fixtures/citation-scorer-NOTES.md`; items 1–3, 13, 15 and 16 are not
/// switches (they are the weights themselves, the float order a port must
/// keep, or the sort, which is stable here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Quirks {
    /// (4, 5) A comma-less author's surname is its last whitespace token:
    /// `"Navarro J. F."` gives `"F."`, `"Planck Collaboration"` gives
    /// `"Collaboration"`. Off: initials (`J.`, `H.-Y.`) are stripped from
    /// both ends and the rest is the surname (`"van der Marel"`, `"Planck
    /// Collaboration"`).
    pub last_token_surname: bool,
    /// (6) The partial-surname match is a raw substring test of the query's
    /// surname inside the candidate's: `"Li"` matches `"Lilly"`, and the
    /// longer surname in the query earns nothing. Off: the shorter surname
    /// must be a whole hyphen- or space-delimited component of the longer,
    /// in either direction.
    pub asymmetric_substring_surname: bool,
    /// (7) `\^{\i}` decodes to `\^ı` with the backslash kept. Off: it is
    /// `î`.
    pub backslash_dotless_i_decode: bool,
    /// (8) No diacritic folding: `Sánchez` never equals `Sanchez`. Off: both
    /// sides are folded (NFD, combining marks dropped) before comparing.
    pub no_diacritic_folding: bool,
    /// (9) Venue containment is unbounded: `"J"` matches `"The Astrophysical
    /// Journal"`. Off: the contained venue must be at least four characters
    /// and sit on word boundaries (so `"Nature"` still matches `"Nature
    /// Physics"`; that is the loose rule's design, not a hole).
    pub loose_venue: bool,
    /// (10) The bibstem key strips dots but keeps spaces, so `"Phys. Rev.
    /// D"` becomes `"PHYS REV D"`, which no entry has; and `AJ` is absent.
    /// Off: spaces are stripped too, and `AJ`, `PHYSREVD`, `PHYSREVLETT`,
    /// `PHYSREV` are in the table.
    pub bibstem_holes: bool,
    /// (11) Title overlap is an absolute count of shared words of four or
    /// more characters (stopwords and numbers included), so a one-word title
    /// that matches perfectly earns 0.05. Off: the fraction of the shorter
    /// title's words found in the other (≥ 75 %: 0.20, ≥ 50 %: 0.12, any:
    /// 0.05). Stopwords and numbers still count either way.
    pub absolute_title_overlap: bool,
    /// (12) The query's own DOI/arXiv/bibcode is never compared with the
    /// candidate's. Off: an equal identifier earns the identifier term by
    /// itself; a differing one earns nothing; with none to compare the
    /// "has any identifier" rule applies.
    pub ignore_identifier_equality: bool,
    /// (12) The identifier term is withheld while the score is zero. Kept on
    /// in `corrected()` too: an unrelated paper that happens to have a DOI
    /// should not edge past the keep filter on the DOI alone, which is what
    /// the Swift comment says and is right.
    pub identifier_bonus_needs_positive_score: bool,
    /// (14) "single-hit" counts raw hits, not kept ones: a 0.60 candidate is
    /// accepted alone but not beside one 0.00 hit. Off: it counts the hits
    /// at or above [`KEEP_THRESHOLD`].
    pub raw_hit_count_for_single: bool,
}

impl Quirks {
    /// Every switch on: reproduces the Swift scorer and passes the goldens.
    pub const fn legacy() -> Self {
        Self {
            last_token_surname: true,
            asymmetric_substring_surname: true,
            backslash_dotless_i_decode: true,
            no_diacritic_folding: true,
            loose_venue: true,
            bibstem_holes: true,
            absolute_title_overlap: true,
            ignore_identifier_equality: true,
            identifier_bonus_needs_positive_score: true,
            raw_hit_count_for_single: true,
        }
    }

    /// Each switch off where the correct behaviour is obvious; see each
    /// field. `identifier_bonus_needs_positive_score` stays on.
    pub const fn corrected() -> Self {
        Self {
            last_token_surname: false,
            asymmetric_substring_surname: false,
            backslash_dotless_i_decode: false,
            no_diacritic_folding: false,
            loose_venue: false,
            bibstem_holes: false,
            absolute_title_overlap: false,
            ignore_identifier_equality: false,
            identifier_bonus_needs_positive_score: true,
            raw_hit_count_for_single: false,
        }
    }
}

impl Default for Quirks {
    fn default() -> Self {
        Self::legacy()
    }
}

// ── Output ──────────────────────────────────────────────────────────────────

/// The terms that fired, with their values. `None` is a term that did not
/// fire (no field to compare, or no match), which carries no penalty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_author: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<f64>,
}

impl Evidence {
    /// The terms in scoring order, named.
    pub fn terms(&self) -> Vec<(&'static str, f64)> {
        [
            ("first_author", self.first_author),
            ("year", self.year),
            ("venue", self.venue),
            ("title", self.title),
            ("identifier", self.identifier),
        ]
        .into_iter()
        .filter_map(|(name, v)| v.map(|v| (name, v)))
        .collect()
    }

    /// The terms added in scoring order; the score is `min(1, sum)`.
    pub fn sum(&self) -> f64 {
        let mut total = 0.0;
        for (_, v) in self.terms() {
            total += v;
        }
        total
    }
}

/// One candidate's score and why.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scored {
    pub score: f64,
    pub evidence: Evidence,
}

/// Which acceptance rule fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// Top ≥ 0.85 with margin ≥ 0.15 over the runner-up.
    HighConfidence,
    /// The only hit, ≥ 0.50.
    SingleHit,
    /// Top ≥ 0.70 with margin ≥ 0.25 over the runner-up.
    Margin,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::HighConfidence => "high-confidence",
            Rule::SingleHit => "single-hit",
            Rule::Margin => "margin",
        }
    }
}

/// What the acceptance rules decided over a list of scored candidates.
/// Indices are into the list given to [`Scorer::accept`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Acceptance {
    /// The auto-accepted candidate, if a rule fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
    /// Candidates at or above [`KEEP_THRESHOLD`], best first (ties keep
    /// input order).
    #[serde(default)]
    pub kept: Vec<usize>,
}

// ── The scorer ──────────────────────────────────────────────────────────────

/// The scorer; stateless apart from its [`Quirks`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scorer {
    pub quirks: Quirks,
}

impl Scorer {
    pub const fn new(quirks: Quirks) -> Self {
        Self { quirks }
    }

    /// Swift's behaviour.
    pub const fn legacy() -> Self {
        Self::new(Quirks::legacy())
    }

    pub const fn corrected() -> Self {
        Self::new(Quirks::corrected())
    }

    /// The query with its LaTeX decoded (authors, title, journal), as the
    /// cascade does once per resolution. Candidates are never decoded.
    pub fn decode(&self, citation: &CitationInput) -> CitationInput {
        let dec = |s: &str| latex::decode_latex_with(s, self.quirks.backslash_dotless_i_decode);
        CitationInput {
            authors: citation.authors.iter().map(|a| dec(a)).collect(),
            title: citation.title.as_deref().map(dec),
            journal: citation.journal.as_deref().map(dec),
            ..citation.clone()
        }
    }

    /// Score one candidate against a citation (decoding the citation first).
    pub fn score(&self, citation: &CitationInput, candidate: &Work) -> Scored {
        self.score_decoded(&self.decode(citation), candidate)
    }

    /// Score against an already-decoded citation (see [`Scorer::decode`]).
    pub fn score_decoded(&self, decoded: &CitationInput, candidate: &Work) -> Scored {
        let q = self.quirks;
        let mut ev = Evidence::default();
        let mut score = 0.0;

        // Author: first author's surname.
        if let (Some(query_author), Some(cand_author)) =
            (decoded.authors.first(), candidate.author.first())
        {
            let q_last = self.key(&self.surname(query_author));
            let c_last = self.key(&self.surname(&author_string(cand_author)));
            if !q_last.is_empty() && q_last == c_last {
                ev.first_author = Some(W_AUTHOR_EXACT);
            } else if !q_last.is_empty() && self.partial_surname(&q_last, &c_last) {
                ev.first_author = Some(W_AUTHOR_PARTIAL);
            }
        }
        if let Some(v) = ev.first_author {
            score += v;
        }

        // Year.
        let cand_year = candidate_year(candidate);
        if let Some(y) = decoded.year {
            if cand_year == Some(y) {
                ev.year = Some(W_YEAR_EXACT);
            } else if let Some(cy) = cand_year {
                if (cy - y).abs() <= 1 {
                    ev.year = Some(W_YEAR_ADJACENT);
                }
            }
        }
        if let Some(v) = ev.year {
            score += v;
        }

        // Venue.
        if let (Some(j), Some(v)) = (
            decoded.journal.as_deref().filter(|j| !j.is_empty()),
            candidate
                .container_title
                .as_deref()
                .filter(|v| !v.is_empty()),
        ) {
            let j_low = self.key(j);
            let v_low = self.key(v);
            let contained = if q.loose_venue {
                v_low.contains(&j_low) || j_low.contains(&v_low)
            } else {
                j_low == v_low
                    || bounded_contains(&v_low, &j_low)
                    || bounded_contains(&j_low, &v_low)
            };
            if contained || self.venue_matches_bibstem(v, j) {
                ev.venue = Some(W_VENUE);
            }
        }
        if let Some(v) = ev.venue {
            score += v;
        }

        // Title overlap.
        if let (Some(t), Some(ct)) = (
            decoded.title.as_deref().filter(|t| !t.is_empty()),
            candidate.title.as_deref().filter(|t| !t.is_empty()),
        ) {
            let query_words = self.title_words(t);
            let cand_words = self.title_words(ct);
            let overlap = query_words.intersection(&cand_words).count();
            ev.title = if q.absolute_title_overlap {
                if overlap >= 5 {
                    Some(W_TITLE_HIGH)
                } else if overlap >= 3 {
                    Some(W_TITLE_MID)
                } else if overlap >= 1 {
                    Some(W_TITLE_LOW)
                } else {
                    None
                }
            } else if overlap == 0 {
                None
            } else {
                let shorter = query_words.len().min(cand_words.len()).max(1);
                let ratio = overlap as f64 / shorter as f64;
                if ratio >= 0.75 {
                    Some(W_TITLE_HIGH)
                } else if ratio >= 0.5 {
                    Some(W_TITLE_MID)
                } else {
                    Some(W_TITLE_LOW)
                }
            };
        }
        if let Some(v) = ev.title {
            score += v;
        }

        // Identifier.
        let has_any = candidate_ids(candidate).iter().any(|(_, v)| v.is_some());
        let gated = !q.identifier_bonus_needs_positive_score || score > 0.0;
        ev.identifier = if q.ignore_identifier_equality {
            (gated && has_any).then_some(W_IDENTIFIER)
        } else {
            match identifier_agreement(decoded, candidate) {
                Some(true) => Some(W_IDENTIFIER),
                Some(false) => None,
                None => (gated && has_any).then_some(W_IDENTIFIER),
            }
        };
        if let Some(v) = ev.identifier {
            score += v;
        }

        Scored {
            score: score.min(1.0),
            evidence: ev,
        }
    }

    /// Candidate indices best first; equal scores keep input order (NOTES
    /// item 16: the Swift order is unspecified, this one is stable).
    pub fn rank(&self, scored: &[Scored]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..scored.len()).collect();
        order.sort_by(|&a, &b| {
            scored[b]
                .score
                .partial_cmp(&scored[a].score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        order
    }

    /// The acceptance rules of the search step, in Swift's order:
    /// high-confidence, single-hit, margin. `raw_hit_count` is how many hits
    /// the source returned before scoring (what "single-hit" counts under
    /// `raw_hit_count_for_single`); pass `scored.len()` when they are the
    /// same list.
    pub fn accept(&self, scored: &[Scored], raw_hit_count: usize) -> Acceptance {
        let ranked = self.rank(scored);
        let kept: Vec<usize> = ranked
            .iter()
            .copied()
            .filter(|&i| scored[i].score >= KEEP_THRESHOLD)
            .collect();
        let Some(&top) = ranked.first() else {
            return Acceptance::default();
        };
        let top_score = scored[top].score;
        let runner_up = ranked.get(1).map(|&i| scored[i].score).unwrap_or(0.0);
        let hits = if self.quirks.raw_hit_count_for_single {
            raw_hit_count
        } else {
            kept.len()
        };
        let rule = if top_score >= ACCEPT_HIGH_TOP && (top_score - runner_up) >= ACCEPT_HIGH_MARGIN
        {
            Some(Rule::HighConfidence)
        } else if hits == 1 && top_score >= ACCEPT_SINGLE_TOP {
            Some(Rule::SingleHit)
        } else if top_score >= ACCEPT_MARGIN_TOP && (top_score - runner_up) >= ACCEPT_MARGIN_MARGIN
        {
            Some(Rule::Margin)
        } else {
            None
        };
        Acceptance {
            accepted: rule.map(|_| top),
            rule,
            kept,
        }
    }

    // ── Text helpers ────────────────────────────────────────────────────

    /// Normalise for comparison: NFC (Swift `==` is canonical-equivalence
    /// aware), optionally diacritic-folded, lowercased.
    fn key(&self, s: &str) -> String {
        let s: String = if self.quirks.no_diacritic_folding {
            s.nfc().collect()
        } else {
            s.nfd().filter(|c| !is_combining_mark(*c)).nfc().collect()
        };
        s.to_lowercase()
    }

    /// `lastName(from:)`: before the comma when there is one; otherwise the
    /// last whitespace token (legacy) or the string without its initials.
    fn surname(&self, author: &str) -> String {
        let trimmed = author.trim();
        if let Some(comma) = trimmed.find(',') {
            return trimmed[..comma].trim().to_string();
        }
        if self.quirks.last_token_surname {
            return trimmed
                .split_whitespace()
                .last()
                .unwrap_or(trimmed)
                .to_string();
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let first = parts.iter().position(|p| !is_initial(p));
        let last = parts.iter().rposition(|p| !is_initial(p));
        match (first, last) {
            (Some(a), Some(b)) => parts[a..=b].join(" "),
            _ => trimmed.to_string(),
        }
    }

    fn partial_surname(&self, q_last: &str, c_last: &str) -> bool {
        if self.quirks.asymmetric_substring_surname {
            return c_last.contains(q_last);
        }
        let (short, long) = if q_last.chars().count() <= c_last.chars().count() {
            (q_last, c_last)
        } else {
            (c_last, q_last)
        };
        !short.is_empty()
            && short != long
            && long
                .split(|c: char| c == '-' || c.is_whitespace())
                .any(|component| component == short)
    }

    /// `venueMatchesBibstem`: the short form the caller sent against the
    /// full name a source returned.
    fn venue_matches_bibstem(&self, venue: &str, bibstem: &str) -> bool {
        let mut bib = bibstem.replace('.', "");
        if !self.quirks.bibstem_holes {
            bib.retain(|c| !c.is_whitespace());
        }
        let bib = bib.trim().to_uppercase();
        let v = self.key(venue);
        let table = if self.quirks.bibstem_holes {
            BIBSTEMS
        } else {
            BIBSTEMS_CORRECTED
        };
        table
            .iter()
            .filter(|(key, _)| *key == bib)
            .flat_map(|(_, needles)| needles.iter())
            .any(|needle| v.contains(needle))
    }

    /// Lowercased alphanumeric runs of four or more characters, as a set.
    /// Swift splits on `CharacterSet.alphanumerics.inverted`, which also
    /// keeps combining marks inside a token; after NFC the goldens have none.
    fn title_words(&self, title: &str) -> BTreeSet<String> {
        self.key(title)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 4)
            .map(str::to_string)
            .collect()
    }
}

/// The Swift expansion table, verbatim: bibstem (dots stripped, uppercased)
/// to lowercase needles searched in the venue.
const BIBSTEMS: &[(&str, &[&str])] = &[
    ("APJ", &["astrophysical journal"]),
    ("APJL", &["astrophysical journal letters"]),
    ("APJS", &["astrophysical journal supplement"]),
    ("MNRAS", &["monthly notices", "mon not r astron soc"]),
    (
        "JCAP",
        &[
            "journal of cosmology and astroparticle",
            "cosmology and astroparticle physics",
        ],
    ),
    ("JHEP", &["journal of high energy physics"]),
    (
        "PHRVD",
        &["physical review d", "phys rev d", "phys. rev. d"],
    ),
    (
        "PHRVL",
        &[
            "physical review letters",
            "phys rev lett",
            "phys. rev. lett",
        ],
    ),
    ("PHRV", &["physical review"]),
    ("NUPHB", &["nuclear physics b"]),
    ("NUPHA", &["nuclear physics a"]),
    ("NATUR", &["nature"]),
    ("NATAS", &["nature astronomy"]),
    ("SCI", &["science"]),
    (
        "A&A",
        &["astronomy and astrophysics", "astronomy & astrophysics"],
    ),
    ("PHLB", &["physics letters b"]),
    ("PHLA", &["physics letters a"]),
    ("PRD", &["physical review d"]),
    ("PRL", &["physical review letters"]),
];

/// [`BIBSTEMS`] plus the holes NOTES item 10 names, keyed with spaces
/// stripped as well as dots.
const BIBSTEMS_CORRECTED: &[(&str, &[&str])] = &[
    ("APJ", &["astrophysical journal"]),
    ("APJL", &["astrophysical journal letters"]),
    ("APJS", &["astrophysical journal supplement"]),
    ("AJ", &["astronomical journal"]),
    ("MNRAS", &["monthly notices", "mon not r astron soc"]),
    (
        "JCAP",
        &[
            "journal of cosmology and astroparticle",
            "cosmology and astroparticle physics",
        ],
    ),
    ("JHEP", &["journal of high energy physics"]),
    (
        "PHRVD",
        &["physical review d", "phys rev d", "phys. rev. d"],
    ),
    (
        "PHYSREVD",
        &["physical review d", "phys rev d", "phys. rev. d"],
    ),
    (
        "PHRVL",
        &[
            "physical review letters",
            "phys rev lett",
            "phys. rev. lett",
        ],
    ),
    (
        "PHYSREVLETT",
        &[
            "physical review letters",
            "phys rev lett",
            "phys. rev. lett",
        ],
    ),
    ("PHRV", &["physical review"]),
    ("PHYSREV", &["physical review"]),
    ("NUPHB", &["nuclear physics b"]),
    ("NUPHA", &["nuclear physics a"]),
    ("NATUR", &["nature"]),
    ("NATAS", &["nature astronomy"]),
    ("SCI", &["science"]),
    (
        "A&A",
        &["astronomy and astrophysics", "astronomy & astrophysics"],
    ),
    ("PHLB", &["physics letters b"]),
    ("PHLA", &["physics letters a"]),
    ("PRD", &["physical review d"]),
    ("PRL", &["physical review letters"]),
];

/// The author as a string in the form the Swift scorer saw: `"Family,
/// Given"`, or the family alone (a comma-less original is stored whole in
/// `family`, so it round-trips).
pub fn author_string(a: &Author) -> String {
    match (a.family.as_deref(), a.given.as_deref()) {
        (Some(f), Some(g)) if !g.is_empty() => format!("{f}, {g}"),
        (Some(f), _) => f.to_string(),
        (None, Some(g)) => g.to_string(),
        (None, None) => String::new(),
    }
}

/// `J.`, `H.-Y.`, `J.-P.`, or a lone letter.
fn is_initial(token: &str) -> bool {
    let bare: String = token.chars().filter(|c| *c != '.' && *c != '-').collect();
    let letters = bare.chars().count();
    if letters == 0 {
        return false;
    }
    if letters == 1 {
        return true;
    }
    // Several letters count as initials only when each carries its dot.
    token.contains('.')
        && token
            .split('-')
            .all(|part| part.chars().count() == 2 && part.ends_with('.'))
}

/// `needle` inside `hay` at word boundaries, four characters or more.
fn bounded_contains(hay: &str, needle: &str) -> bool {
    if needle.chars().count() < 4 {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = hay[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = hay[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn candidate_year(work: &Work) -> Option<i32> {
    work.issued
        .as_ref()
        .and_then(|i| i.date_parts.first())
        .and_then(|p| p.first())
        .copied()
}

/// The candidate's DOI, arXiv id and bibcode, each `Some` only when
/// non-empty (an empty string on a candidate is no identifier).
fn candidate_ids(work: &Work) -> [(&'static str, Option<&str>); 3] {
    fn non_empty(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty())
    }
    [
        (
            "doi",
            non_empty(
                work.doi
                    .as_deref()
                    .or(work.ids.get("doi").map(String::as_str)),
            ),
        ),
        (
            "arxiv",
            non_empty(work.ids.get("arxiv").map(String::as_str)),
        ),
        (
            "bibcode",
            non_empty(work.ids.get("bibcode").map(String::as_str)),
        ),
    ]
}

/// `Some(true)` when any identifier both sides carry agrees, `Some(false)`
/// when the ones they share all differ, `None` when they share none.
fn identifier_agreement(decoded: &CitationInput, candidate: &Work) -> Option<bool> {
    let query = [
        ("doi", decoded.doi.as_deref()),
        ("arxiv", decoded.arxiv.as_deref()),
        ("bibcode", decoded.bibcode.as_deref()),
    ];
    let mut seen = None;
    for ((kind, q), (_, c)) in query.iter().zip(candidate_ids(candidate)) {
        let (Some(q), Some(c)) = (q.filter(|s| !s.is_empty()), c) else {
            continue;
        };
        let equal = normalise_id(kind, q) == normalise_id(kind, c);
        if equal {
            return Some(true);
        }
        seen = Some(false);
    }
    seen
}

fn normalise_id(kind: &str, value: &str) -> String {
    let v = value.trim();
    match kind {
        "doi" => v
            .trim_start_matches("https://doi.org/")
            .trim_start_matches("http://dx.doi.org/")
            .trim_start_matches("doi:")
            .to_lowercase(),
        "arxiv" => v.trim_start_matches("arXiv:").to_lowercase(),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author(s: &str) -> Author {
        match s.split_once(',') {
            Some((f, g)) => Author {
                family: Some(f.trim().into()),
                given: Some(g.trim().into()),
                orcid: None,
            },
            None => Author {
                family: Some(s.into()),
                given: None,
                orcid: None,
            },
        }
    }

    #[test]
    fn surname_legacy_and_corrected() {
        let legacy = Scorer::legacy();
        let fixed = Scorer::corrected();
        assert_eq!(legacy.surname("Bardeen, J. M."), "Bardeen");
        assert_eq!(legacy.surname("J. F. Navarro"), "Navarro");
        assert_eq!(legacy.surname("Navarro J. F."), "F.");
        assert_eq!(legacy.surname("R. P. van der Marel"), "Marel");
        assert_eq!(fixed.surname("Navarro J. F."), "Navarro");
        assert_eq!(fixed.surname("R. P. van der Marel"), "van der Marel");
        assert_eq!(fixed.surname("Schive H.-Y."), "Schive");
        assert_eq!(
            fixed.surname("Planck Collaboration"),
            "Planck Collaboration"
        );
        assert_eq!(fixed.surname("Y. Li"), "Li");
        assert_eq!(author_string(&author("Navarro J. F.")), "Navarro J. F.");
    }

    #[test]
    fn partial_surname_rules() {
        let legacy = Scorer::legacy();
        let fixed = Scorer::corrected();
        assert!(legacy.partial_surname("li", "lilly"));
        assert!(!legacy.partial_surname("navarro-gonzalez", "navarro"));
        assert!(!fixed.partial_surname("li", "lilly"));
        assert!(fixed.partial_surname("navarro-gonzalez", "navarro"));
        assert!(fixed.partial_surname("navarro", "navarro-gonzalez"));
        assert!(fixed.partial_surname("planck", "planck collaboration"));
    }

    #[test]
    fn bounded_venue_containment() {
        assert!(!bounded_contains("the astrophysical journal", "j"));
        assert!(bounded_contains("nature physics", "nature"));
        assert!(!bounded_contains("supernature", "nature"));
    }

    #[test]
    fn bibstem_holes_are_a_switch() {
        let legacy = Scorer::legacy();
        let fixed = Scorer::corrected();
        assert!(!legacy.venue_matches_bibstem("Physical Review D", "Phys. Rev. D"));
        assert!(fixed.venue_matches_bibstem("Physical Review D", "Phys. Rev. D"));
        assert!(!legacy.venue_matches_bibstem("The Astronomical Journal", "AJ"));
        assert!(fixed.venue_matches_bibstem("The Astronomical Journal", "AJ"));
        assert!(legacy.venue_matches_bibstem("The Astrophysical Journal", "ApJ"));
    }

    #[test]
    fn diacritic_folding_is_a_switch() {
        assert_ne!(
            Scorer::legacy().key("Sánchez"),
            Scorer::legacy().key("Sanchez")
        );
        assert_eq!(
            Scorer::corrected().key("Sánchez"),
            Scorer::corrected().key("Sanchez")
        );
        // NFD and NFC agree either way.
        assert_eq!(
            Scorer::legacy().key("Lemai\u{302}tre"),
            Scorer::legacy().key("Lemaître")
        );
    }
}
