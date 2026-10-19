# citation-scorer-goldens: decisions that look wrong

`citation-scorer-goldens.json` pins what `AutomationService.scoreCandidate`
(plus the step-3 acceptance rules of `resolveStructuredCitation`) does today.
Scores were recorded by running the Swift scorer. Nothing here is a spec: the
port to Rust (koine plan-source-convention S-D6) should decide each item below
on purpose, not inherit it. Case names are the fixture's `name`.

## Weights as found in code (they match the survey)

first-author surname equal 0.25 (candidate surname *contains* query surname 0.15);
year equal 0.20 (candidate within 1 year, when not equal, 0.08);
venue 0.20 (substring either way, or bibstem table);
title words of >= 4 chars, as a set: overlap 1-2 -> 0.05, 3-4 -> 0.12, >= 5 -> 0.20;
any non-empty doi/arxiv/bibcode on the candidate 0.15, **only if the score is already > 0**; capped at 1.0.
Acceptance (ADS step only): top >= 0.85 and margin >= 0.15 ("high-confidence");
exactly one ADS hit with >= 0.50 ("single-hit"); top >= 0.70 and margin >= 0.25 ("margin");
else candidates >= 0.30 are kept. Differences from the brief: the identifier bonus
condition (score > 0), the title rule counts words not a ratio, and the rules apply
to ADS only. The all-sources fallback (step 4) keeps >= 0.30 but never auto-accepts, and
the local-text step uses its own threshold of 0.55, which is not pinned here.
The fixture's `kept` is the >= 0.30 filter in score order.

## Recorded decisions that look wrong

1. Wrong paper auto-accepted on author+year alone: single-hit takes 0.60-0.65 with a
   different title, venue and no title overlap (`single_hit_0_60_wrong_paper_same_author_year`, `partial_surname_false_positive_white`).
   Nothing penalises a title mismatch.
2. The right paper loses to a sibling: Planck 2018 VI vs VIII score 1.0 vs 0.92 (same collaboration, year, venue, shared words "planck/2018/results"), so the exact match is not accepted (`exact_planck_two_candidates`).
3. Preprint and published copies of one paper compete: 0.80 vs 0.88 when the citation carries the preprint year,
   so neither is accepted although they are the same work (`preprint_vs_published_query_cites_preprint_year`). Nothing collapses them.
4. `lastName` takes the last whitespace token: "Navarro J. F." gives "F." (`author_initials_trailing_no_comma`, 1.0 -> 0.75);
   "R. P. van der Marel" gives "Marel" and never matches "van der Marel, R. P." (`author_prefix_surname_first_format`).
5. Collaborations: ADS "Planck Collaboration" has surname "Collaboration", so query "Planck" misses (`author_collaboration_surname_only`)
   while any "X Collaboration" equals any "Y Collaboration" (`author_collaboration_full_string`).
6. Partial surname is raw substring, no word boundary: "Li" matches "Lilly", "White" matches "Whiteside" (`partial_surname_short_name_li`, `partial_surname_false_positive_white`).
   It is also asymmetric: the longer surname in the query earns nothing (`partial_surname_reversed_not_matched`).
7. `decodeLatex` leaves `\^{\i}` as "Lema\^ıtre" (dotless i, backslash kept), so the standard LaTeX spelling of Lemaître never matches its own unicode form
   (`latex_query_decoded_vs_unicode_candidate`). Only the query is decoded; a LaTeX-encoded candidate author misses (`latex_candidate_not_decoded`).
8. No diacritic folding: "Sánchez" vs "Sanchez" and "Lemaître" vs "Lemaitre" miss (`unicode_sanchez_vs_ascii_candidate`, `unicode_query_vs_ascii_candidate`).
   Conversely NFD and NFC match only because Swift `==` is canonical-equivalence-aware (`unicode_nfd_candidate_vs_nfc_query`); a byte-wise Rust compare would silently differ.
9. Venue substring matching is loose: query "J" matches "The Astrophysical Journal" (`venue_short_query_contained_in_longer_name`),
   "Nature" matches Nature Physics/Astronomy, "Science" matches Science Advances, "ApJ" matches Letters and Supplement (`venue_*`).
10. The bibstem table has holes: "AJ" and the dotted forms "Phys. Rev. D", "Phys. Rev. Lett." never match (`venue_aj_not_in_bibstem_table`,
    `venue_phys_rev_d_dotted_form_misses`, `venue_phys_rev_lett_dotted_form`), because the dots are stripped but the spaces kept ("PHYS REV D" is not a key).
11. Title overlap is an absolute count of >= 4-char tokens, not a fraction: a one-word title that matches perfectly earns at most 0.05, while 5 coincidental words earn 0.20;
    stopwords ("with", "that"), years and numbers all count (`title_overlap_*`, `title_substring_not_matched`).
12. The input DOI/arXiv/bibcode is never compared with the candidate's: a candidate carrying the very DOI in the query scores 0 if it differs elsewhere
    (`query_doi_matches_but_wrong_paper`); the bonus is for having *any* identifier, and is withheld at score 0 (`title_overlap_0_words_with_identifier_gets_no_bonus`).
13. Missing data is not penalised: empty query authors, empty candidate authors or a missing year still reach single-hit acceptance at 0.72-0.75
    (`author_empty_*`, `candidate_year_missing`, `exact_without_query_journal`). A candidate within 2 years still reaches high-confidence-adjacent 0.80 (`year_off_by_two`).
14. The single-hit rule counts raw ADS hits, not kept ones: the same 0.60 candidate is accepted alone but not next to one junk 0.00 hit (`single_hit_between_0_50_and_0_70` vs `two_hits_top_below_0_70`).
15. Thresholds sit on float sums: 0.85 is recorded as 0.8500000000000001, a 0.15 margin as 0.15000000000000002, and 0.30 / 0.50 pass `>=` only because the additions happen to land exactly
    (`margin_rule_0_15_float_boundary_0_85_vs_0_70`, `kept_threshold_exactly_0_30_*`, `single_hit_exactly_0_50_boundary`). A port must add in the same order (author, year, venue, title, identifier) in f64.
16. Equal top scores are ranked by Swift's unspecified `sorted` order; the fixture's `accepted` index breaks ties by input order, but no accepted case has a top tie
    (`margin_too_small_two_equal_candidates` is rejected either way).
