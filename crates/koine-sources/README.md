# koine-sources

The source convention's vocabulary (`docs/plan-source-convention.md`, work
package S1): how an online database declares itself to koine. Types only; no
HTTP and no adapters. An adapter crate depends on this one.

| Module | What |
|---|---|
| `explain` | `ExplainRecord`: indexes, relations, sortable, identifiers, fills, page, etiquette, secrets, native (S-D1) |
| `query` | the `bib.*` indexes and relations, the JSON `Query`, `covered_by` (S-D2) |
| `kind` | `bibliographic-work@1.0.0`: the schema, the typed `Work`, `to_csl_json` (S-D4) |
| `diagnostics` | `Diagnostic { source, status, detail }` (S-D7) |
| `score` | the citation-resolution `Scorer`, its `Quirks`, `Evidence`, the acceptance rules (S-D6) |
| `cascade` | `resolve` over `LocalLookup` + `RemoteSearch` trait objects; `source_priority`; `dedup_by_identifier` (S-D6) |
| `latex` | the query-side LaTeX decoder (a port of `im-bibtex`'s), with its one quirk as a flag |

The kind is spelled `bibliographic-work@1.0.0` in `schema-refs.json`, like
every versioned kind; an explain record may say `bibliographic-work@1`.
Non-CSL fields (`ids`, `locations`, `provenance`, `metrics`) sit beside the CSL
body; `to_csl_json` emits strict CSL with them under `custom`.

## 1. An explain record

```json
{"source": "crossref", "name": "Crossref", "version": "rest-v1",
 "kind": "bibliographic-work@1", "operations": ["search", "fetch"],
 "indexes": {"title": {"relations": ["any", "adj"]},
             "doi":   {"relations": ["="], "exact": true}},
 "identifiers": ["doi"], "page": {"model": "cursor", "max": 1000}}
```

```rust
let record = koine_sources::ExplainRecord::from_json(text)?; // parses and validates
```

## 2. A query and its diagnostics

```rust
let q: Query = serde_json::from_str(
    r#"{"and":[{"index":"first_author","relation":"any","term":"Abel"},
              {"index":"title","relation":"any","term":"reionization"}]}"#)?;
let diagnostics = q.covered_by(&record);
// [{"source":"crossref","status":"unsupported","detail":"bib.first_author"}]
```

An index the record does not declare is a visible diagnostic, never a
silent fallback to another index.

## 3. A work, as stored and as strict CSL

```rust
koine_sources::ensure_registered();            // the kind, once per process
let work: Work = serde_json::from_value(row)?; // a bad location version is refused
let csl = work.to_csl_json();                  // CSL keys; ids/locations under `custom`
```

## 4. The scorer (S-D6, work package S4)

`Scorer` is imbib's `AutomationService.scoreCandidate` and the auto-accept
closure of `resolveStructuredCitation`, ported from Swift and pinned
bit-exactly by `fixtures/citation-scorer-goldens.json` — 98 cases recorded
from the Swift scorer (S2a) — under `Quirks::legacy()`. The goldens pin
*behaviour*, not correctness: `fixtures/citation-scorer-NOTES.md` lists the
sixteen recorded decisions that look wrong, and every one that is arithmetic
is a named switch in `Quirks`. Nothing is fixed silently.

```rust
let scorer = Scorer::legacy();                       // or Scorer::corrected()
let scored = scorer.score(&citation, &work);          // Scored { score, evidence }
// evidence: {first_author: 0.25, year: 0.20, venue: 0.20, title: 0.20, identifier: 0.15}
let decision = scorer.accept(&all_scored, raw_hit_count);
// Acceptance { accepted: Some(i), rule: Some(HighConfidence), kept: [i, ...] }
```

Weights: first-author surname 0.25 (candidate's contains the query's: 0.15);
year 0.20 (within one: 0.08); venue 0.20 (substring either way, or the
bibstem table); title words of four or more characters as a set, shared 1–2:
0.05, 3–4: 0.12, 5 or more: 0.20; a usable identifier 0.15, only when the
score is already above zero; capped at 1.0. Rules, in order: top ≥ 0.85 with
margin ≥ 0.15 (`high-confidence`); the only hit, ≥ 0.50 (`single-hit`); top
≥ 0.70 with margin ≥ 0.25 (`margin`). `kept` is everything ≥ 0.30, best
first, ties in input order. The terms are added in that order in f64 and
compared with `>=`, because the thresholds sit on float sums
(0.8500000000000001). Strings are NFC-normalised first: Swift `==` is
canonical-equivalence aware. LaTeX is decoded on the query side only
(`Scorer::decode`), never on a candidate.

`min(1, evidence.sum()) == score`, exactly; the golden test asserts it for
every candidate.

### The quirks

`true` is the Swift behaviour; `Quirks::legacy()` is all `true`.

| Switch | NOTES | Legacy (`true`) | Corrected (`false`) |
|---|---|---|---|
| `last_token_surname` | 4, 5 | comma-less author's surname is its last token: `"Navarro J. F."` → `F.`, `"Planck Collaboration"` → `Collaboration` | initials stripped from both ends: `Navarro`, `van der Marel`, `Planck Collaboration` |
| `asymmetric_substring_surname` | 6 | raw substring, query inside candidate only: `Li` matches `Lilly`; `Navarro-Gonzalez` vs `Navarro` earns nothing | the shorter must be a whole `-`/space component of the longer, either direction |
| `backslash_dotless_i_decode` | 7 | `\^{\i}` decodes to `\^ı` | `î` (and `ì`, `ï`) |
| `no_diacritic_folding` | 8 | `Sánchez` ≠ `Sanchez` | folded (NFD, marks dropped) before comparing |
| `loose_venue` | 9 | `"J"` matches `"The Astrophysical Journal"` | contained venue ≥ 4 chars, on word boundaries (`Nature` still matches `Nature Physics`) |
| `bibstem_holes` | 10 | key strips dots, keeps spaces: `Phys. Rev. D` → `PHYS REV D`, no entry; `AJ` absent | spaces stripped too; `AJ`, `PHYSREVD`, `PHYSREVLETT`, `PHYSREV` added |
| `absolute_title_overlap` | 11 | absolute count of shared ≥4-char words | fraction of the *shorter* title's words: ≥ 75 % 0.20, ≥ 50 % 0.12, any 0.05 |
| `ignore_identifier_equality` | 12 | the query's DOI/arXiv/bibcode is never compared | equal id earns 0.15 by itself; differing earns nothing; none shared → the "has any" rule |
| `identifier_bonus_needs_positive_score` | 12 | 0.15 withheld at score 0 | **stays `true` in `corrected()`**: a DOI alone should not pass the keep filter |
| `raw_hit_count_for_single` | 14 | `single-hit` counts raw hits: 0.60 beside one 0.00 is not accepted | counts hits ≥ 0.30 |

Not switches: items 1–3 (no title-mismatch penalty, siblings, preprint vs
published — the weights and the merge, S-D7), 13 (missing fields carry no
penalty), 15 (the float order, which a port keeps) and 16 (ties: the sort
here is stable by input index). Item 7's second half — a LaTeX-encoded
*candidate* is never decoded — is by design: sources send Unicode.

`tests/scorer_goldens.rs::corrected_changes_exactly_these_decisions` is the
list of golden cases whose decision (`accepted`, `rule`, `kept`) moves under
`Quirks::corrected()` — sixteen of 98, each annotated with the switch that
moved it. A change to that list is a change to the scorer.

### The cascade

`cascade::resolve(&scorer, &citation, &local, &remote)` is
`resolveStructuredCitation` with the I/O behind two traits, so it is a pure
function a stub can drive: identifier (field, or scanned from the BibTeX or
free text; local then remote; `via: identifier`, score 1), local text (top hit
≥ 0.55, `via: local`), then search (scored, ranked, put to the rules, `via:
search`). Below threshold the candidates come back with scores, evidence and
no `accepted` — nothing auto-merges. Swift's fourth step (all-sources free-text
fallback) is the federation's (S-D7), not a step here.

### The priority table

One table, `cascade::source_priority`, replacing Swift's
`deduplicationPriority` and `imbib-core`'s `SOURCE_PRIORITY`, which disagreed:

| source | priority |
|---|---|
| crossref | 10 |
| pubmed | 20 |
| wos | 25 |
| ads | 30 |
| scix | 31 |
| s2 (`semanticscholar`) | 40 |
| openalex | 50 |
| arxiv | 60 |
| dblp | 70 |
| anything else | 100 |

The Rust table is the base; the two sources only Swift ranked are slotted
where Swift had them relative to ADS (scix just behind it; wos ahead, as a
curated index). Swift's numbers for the shared sources (crossref 40, pubmed
50, behind ADS) are dropped: the publisher source wins a DOI-bearing merge.
`dedup_by_identifier` applies it.

### Re-recording the goldens

Only after an intentional change to the Swift scorer, on the imbib side:

```
CITATION_GOLDEN_RECORD_IN=<cases.json> CITATION_GOLDEN_RECORD_OUT=<out.json> \
  swift test --filter CitationScorerGolden          # in apps/imbib/PublicationManagerCore
```

(`CitationScorerGoldenTests.swift`). Copy the result over
`fixtures/citation-scorer-goldens.json`, update `citation-scorer-NOTES.md`,
and expect `legacy_reproduces_every_golden` to tell you which switch the Swift
change corresponds to. A golden that fails after a deliberate scorer change
may itself be wrong (S-D6): verify it against the source, then edit it in
place.
