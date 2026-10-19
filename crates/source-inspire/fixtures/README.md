# Recorded INSPIRE-HEP responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against `https://inspirehep.net/api` (keyless), every one
with `curl -sS -D <name>.headers -A 'koine-source-inspire/0.1 (mailto:tabel@stanford.edu)' -G … -o <name>.json`
and the `fields` value below. The Tier A tests in `src/tests.rs` serve these
from a mock server; nothing in `cargo test` reaches inspirehep.net. INSPIRE
sends no `X-RateLimit-*` headers on these responses (it lists them as exposed
but they were absent), so the 15 requests / 5 s burst limit is the adapter's
declaration, from INSPIRE's documentation.

`FIELDS` = `control_number,titles,authors.full_name,authors.first_name,authors.last_name,authors.ids,earliest_date,preprint_date,publication_info,dois,arxiv_eprints,documents,abstracts,citation_count,document_type,external_system_identifiers,collaborations,languages`
(`crate::FIELDS`). The single-record routes (`/doi/…`, `/arxiv/…`,
`/literature/<recid>`) honour it only partly: they still return the extra
`references`, `keywords`, `urls` and similar keys, which the parser ignores.

| File | Request (base `https://inspirehep.net/api`) |
|---|---|
| `search.json` | `/literature` `--data-urlencode 'q=a Abel and (t first and t stars)' size=3 page=1 sort=mostcited fields=$FIELDS` — 7 hits, 3 sent; the query the adapter builds for author Abel + title "first stars" |
| `year.json` | `/literature` `q=fa Abel and date 2002 size=2 page=1 fields=$FIELDS` — 8 hits |
| `cited-by.json` | `/literature` `q=refersto:recid:728142 size=2 page=1` — 99 citing works (`related cited_by`) |
| `references.json` | `/literature` `q=citedby:recid:728142 size=2 page=1` — 42 references (`related cites`) |
| `fetch-doi.json` | `/doi/10.1126/science.1063991` `fields=$FIELDS` (Abel, Bryan & Norman 2002; recid 578281) |
| `fetch-arxiv.json` | `/arxiv/astro-ph/0112088` `fields=$FIELDS` — the same record by arXiv id |
| `fetch-recid.json` | `/literature/728142` `fields=$FIELDS` (Gao et al. 2007; has `documents`, `report_numbers`, `keywords`) |
| `export.bib` | `/literature/728142` `format=bibtex` — INSPIRE's own BibTeX (`Gao:2006ug`) |
| `not-found.json` | `/literature/999999999` `fields=$FIELDS` — HTTP 404, `PIDDoesNotExistRESTError` |
| `bad-query.json` | `/literature` `q=((` `size=2` — HTTP 200 with zero hits: an unparseable query is not an error to INSPIRE |
| `collaboration.json` | `/literature` `q=collaboration CMS and t "observation of a new boson" and date 2012 size=1 fields=$FIELDS` — **recorded, then cut**: the record has 2,900 authors (686 KB); the file keeps the first 60 so that the "more than 50 authors with a collaboration → the collaboration" rule is exercised; every other byte is as received |

Each `<name>.headers` is the response head as received.

## Facts the fixtures do not show (measured 2026-10-19, `size=1`, `fields=control_number`)

- `date > 2002` and `date < 2002` are not complementary to `date 2002` (487 /
  81 / 16 for author Abel, against 571 in all), and the open-ended `date
  2003->` matched nothing; the adapter therefore maps `year` `<`/`>` to
  closed ranges (`date 1000->2001`, `date 2003->2999`), and `within` to
  `date 2020->2024`, which agreed with `date 2000->2002` (42) and `date
  2002->2002` (16 = `date 2002`).
- `a "Tom Abel"` (a quoted name) matched nothing, `a Tom Abel` 165 and
  `a "Abel, Tom"` 152: names are never quoted; a name with a comma goes
  through as written, otherwise each word gets its own `a`.
- `refersto:arxiv:astro-ph/0112088` matched 677,185 records (nothing useful),
  `refersto:recid:728142` 99: citation walks are by recid only.
- `?format=bibtex` on the literature record route answers `application/x-bibtex`
  text without an `Accept` header.
