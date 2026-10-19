# Recorded Semantic Scholar responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against `https://api.semanticscholar.org`, **without an
API key**, with `curl -sS -D <name>.h -A 'koine-source-s2/0.1 (mailto:tabel@stanford.edu)' -G <url> … -o <name>.j`
(then renamed to `<name>.headers` / `<name>.json`). The keyless pool is shared
and answered `429` to about nine calls in ten while these were recorded; the
recording script retried only on 429, after 6 s, and kept the first
non-429 answer. The Tier A tests in `src/tests.rs` serve these from a mock
server; nothing in `cargo test` reaches api.semanticscholar.org.

`F` = `paperId,corpusId,externalIds,title,abstract,venue,publicationVenue,year,publicationDate,citationCount,isOpenAccess,openAccessPdf,publicationTypes,journal,authors`
(the adapter sends the same set, `crate::FIELDS`; the order differs from `LIST_FIELDS`
used for related lists, which drops the abstract).
`L` = `paperId,corpusId,externalIds,title,year,citationCount,authors,venue,isOpenAccess,openAccessPdf,publicationTypes,journal`

| File | Request |
|---|---|
| `search.json` | `GET /graph/v1/paper/search` `query=attention is all you need fields=$F offset=0 limit=3` — `total` 7,492, `next` 3 |
| `year-search.json` | `GET /graph/v1/paper/search` `query=first stars universe year=2002-2003 fields=$F offset=0 limit=2` — two review papers; one with a BRONZE publisher pdf, one with an arXiv id |
| `fetch-arxiv.json` | `GET /graph/v1/paper/ARXIV:1706.03762` `fields=$F` — the Transformer paper (`openAccessPdf.url` is the empty string, `isOpenAccess` false) |
| `fetch-corpusid.json` | `GET /graph/v1/paper/CorpusId:13756489` `fields=$F` — the same paper |
| `fetch-doi.json` | `GET /graph/v1/paper/DOI:10.18653/v1/N19-1423` `fields=$F` — BERT: a DOI, a HYBRID `openAccessPdf` (the DOI's own landing page, in lower case) and an arXiv id |
| `citations.json` | `GET /graph/v1/paper/ARXIV:1706.03762/citations` `fields=$L offset=0 limit=2` — rows are `{"citingPaper": …}` |
| `references.json` | `GET /graph/v1/paper/ARXIV:1706.03762/references` `fields=$L offset=0 limit=2` — rows are `{"citedPaper": …}` |
| `recommendations.json` | `GET /recommendations/v1/papers/forpaper/204e3073870fae3d05bcbc2f6a8e263d9b72e776` `fields=$L limit=2 from=all-cs` |
| `not-found.json` | `GET /graph/v1/paper/DOI:10.1126/science.1063991` `fields=$F` — HTTP 404, `{"error":"Paper with id DOI:… not found"}` (S2 does not hold Abel, Bryan & Norman 2002 under that DOI) |
| `rate-limited-429.json` | the keyless pool's 429, **recorded** (not hand-written): `GET /graph/v1/paper/search` `query=first stars universe Abel …`; no `Retry-After` header, so the adapter backs off for its 10 s default |

Each `<name>.headers` is the response head as received.

## Facts the fixtures do not show

- The recommendations API answers keyless but only from a pool: with no
  `from`, the Transformer paper got `{"recommendedPapers": []}` (HTTP 200);
  with `from=all-cs` it got the rows above. `forpaper/ARXIV:1706.03762` is
  empty too: the path wants a paper id. The adapter resolves one first and
  sends `from=all-cs`.
- `/paper/search` serves the first 1,000 hits only (documented); the adapter
  refuses to ask past that rather than relay an error.
