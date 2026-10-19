//! The pin of docs/plan-source-convention.md S-D6: the 98 goldens recorded
//! from imbib's Swift scorer (work package S2a) reproduce under
//! `Quirks::legacy()`, and `Quirks::corrected()` changes exactly the
//! documented list of decisions.

use std::collections::BTreeMap;

use koine_sources::kind::{Author, Issued, Work};
use koine_sources::{Acceptance, CitationInput, Quirks, Rule, Scorer};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct GoldenCandidate {
    title: String,
    authors: Vec<String>,
    year: Option<i32>,
    venue: Option<String>,
    doi: Option<String>,
    arxiv: Option<String>,
    bibcode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    citation: CitationInput,
    candidates: Vec<GoldenCandidate>,
    scores: Vec<f64>,
    accepted: Option<usize>,
    rule: Option<String>,
    #[serde(default)]
    kept: Vec<usize>,
}

fn cases() -> Vec<Case> {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../fixtures/citation-scorer-goldens.json")).unwrap();
    assert_eq!(cases.len(), 98);
    cases
}

/// The golden's author strings into CSL authors, so that `author_string`
/// gives the Swift scorer's input back: `"Family, Given"` splits, anything
/// else is the family whole.
fn author(s: &str) -> Author {
    match s.split_once(',') {
        Some((f, g)) => Author {
            family: Some(f.trim().to_string()),
            given: Some(g.trim().to_string()),
            orcid: None,
        },
        None => Author {
            family: Some(s.to_string()),
            given: None,
            orcid: None,
        },
    }
}

fn work(c: &GoldenCandidate) -> Work {
    let mut ids = BTreeMap::new();
    for (k, v) in [
        ("doi", &c.doi),
        ("arxiv", &c.arxiv),
        ("bibcode", &c.bibcode),
    ] {
        if let Some(v) = v {
            ids.insert(k.to_string(), v.clone());
        }
    }
    Work {
        ty: "article-journal".into(),
        title: Some(c.title.clone()),
        author: c.authors.iter().map(|a| author(a)).collect(),
        issued: c.year.map(|y| Issued {
            date_parts: vec![vec![y]],
        }),
        container_title: c.venue.clone(),
        doi: c.doi.clone(),
        ids,
        ..Work::default()
    }
}

fn run(scorer: &Scorer, c: &Case) -> (Vec<f64>, Acceptance) {
    let scored: Vec<_> = c
        .candidates
        .iter()
        .map(|cand| scorer.score(&c.citation, &work(cand)))
        .collect();
    for s in &scored {
        // (c) evidence sums to the score, exactly.
        assert_eq!(
            s.evidence.sum().min(1.0),
            s.score,
            "{}: evidence != score",
            c.name
        );
    }
    let acceptance = scorer.accept(&scored, c.candidates.len());
    (scored.iter().map(|s| s.score).collect(), acceptance)
}

fn rule_name(r: Option<Rule>) -> Option<String> {
    r.map(|r| r.as_str().to_string())
}

/// (a) The pin: every case, to 1e-12, under the legacy quirks.
#[test]
fn legacy_reproduces_every_golden() {
    let scorer = Scorer::new(Quirks::legacy());
    let mut passed = 0;
    for c in cases() {
        let (scores, acc) = run(&scorer, &c);
        assert_eq!(scores.len(), c.scores.len(), "{}", c.name);
        for (i, (got, want)) in scores.iter().zip(&c.scores).enumerate() {
            assert!(
                (got - want).abs() <= 1e-12,
                "{} candidate {i}: got {got:?}, golden {want:?}",
                c.name
            );
        }
        assert_eq!(acc.accepted, c.accepted, "{} accepted", c.name);
        assert_eq!(rule_name(acc.rule), c.rule, "{} rule", c.name);
        assert_eq!(acc.kept, c.kept, "{} kept", c.name);
        passed += 1;
    }
    assert_eq!(passed, 98);
}

/// (b) What fixing the scorer changes: the cases whose decision (accepted,
/// rule, kept) differs under `Quirks::corrected()`. This list is the first
/// honest statement of the effect of the corrections; a change to it is a
/// change to the scorer and is reviewed as one.
#[test]
fn corrected_changes_exactly_these_decisions() {
    // Fixture order. The switch, and what moved.
    const CHANGED: &[&str] = &[
        // bibstem_holes: AJ now matches; 0.80 -> 1.00, margin -> high-confidence.
        "exact_riess_vs_perlmutter",
        // asymmetric_substring_surname: "White" no longer inside "Whiteside";
        // 0.60 -> 0.45, the wrong paper is no longer auto-accepted (NOTES 1).
        "partial_surname_false_positive_white",
        // asymmetric_substring_surname: "Li" no longer inside "Lilly"; 0.90 -> 0.75.
        "partial_surname_short_name_li",
        // asymmetric_substring_surname: symmetric now; 0.75 -> 0.90.
        "partial_surname_reversed_not_matched",
        // last_token_surname: "Navarro J. F." -> "Navarro"; 0.75 -> 1.00.
        "author_initials_trailing_no_comma",
        // last_token_surname: "Planck" is a component of "Planck Collaboration"; 0.75 -> 0.90.
        "author_collaboration_surname_only",
        // last_token_surname: "van der Marel"; 0.52 -> 0.85.
        "author_prefix_surname_first_format",
        // no_diacritic_folding: Sánchez == Sanchez; 0.75 -> 1.00.
        "unicode_sanchez_vs_ascii_candidate",
        // bibstem_holes: 0.80 -> 1.00, single-hit -> high-confidence.
        "venue_aj_not_in_bibstem_table",
        "venue_phys_rev_lett_dotted_form",
        "venue_phys_rev_d_dotted_form_misses",
        // loose_venue: "J" no longer matches "The Astrophysical Journal"; 1.00 -> 0.80.
        "venue_short_query_contained_in_longer_name",
        // absolute_title_overlap: 3 of the shorter title's 3 words is 100 %;
        // 0.27 -> 0.35, now kept.
        "title_overlap_3_words_with_identifier",
        // absolute_title_overlap: the runner-up's 3 shared words are now 0.20;
        // 0.27 -> 0.35, so it is kept beside the accepted top.
        "accept_high_confidence_clear_margin",
        // raw_hit_count_for_single: one kept hit beside a 0.00 is single-hit, which
        // is tried before margin; same acceptance, rule name changes.
        "two_hits_top_0_80_margin_0_80_accepts_via_margin_rule",
        // raw_hit_count_for_single: the 0.60 beside junk is now accepted (NOTES 14).
        "two_hits_top_below_0_70",
    ];
    let scorer = Scorer::new(Quirks::corrected());
    let mut changed = Vec::new();
    for c in cases() {
        let (_, acc) = run(&scorer, &c);
        if acc.accepted != c.accepted || rule_name(acc.rule) != c.rule || acc.kept != c.kept {
            changed.push(c.name.clone());
        }
    }
    assert_eq!(changed, CHANGED);
}
