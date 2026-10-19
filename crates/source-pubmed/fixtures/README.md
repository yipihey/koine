# Recorded PubMed (NCBI E-utilities) responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against `https://eutils.ncbi.nlm.nih.gov/entrez/eutils`
(E-utilities 2.0, `retmode=json`) with
`curl -A "koine-source-pubmed/0.1 (mailto:tabel@stanford.edu)"` and
`tool=koine-source-pubmed&email=tabel@stanford.edu`, **no API key** (the
anonymous 3 req/s tier; `x-ratelimit-limit: 3` in each `.headers` file is the
proof). The Tier A tests in `src/tests.rs` serve these from a mock server;
nothing in `cargo test` reaches NCBI except the one `#[ignore]` live test.

Every `.json` has its `.headers` beside it. Request lines, all `GET`, all
with the `tool`/`email` pair above:

| File | Request |
|---|---|
| `search.json` | `esearch.fcgi?db=pubmed&retmode=json&retmax=3&retstart=0&sort=relevance&term=(Doudna[Author] AND CRISPR[Title] AND 2014:2014[PDAT])` — exactly the `term` `query.rs` writes for author + title + `year =`; `count` 10, so a page of 3 has a `next` |
| `summary.json` | `esummary.fcgi?db=pubmed&retmode=json&id=25430774,24476820,25468820` — the three ids `search.json` returned, in that order (Doudna & Charpentier *Science* 2014; Sternberg et al. *Nature* 2014, in PMC; Hochstrasser & Doudna *Trends Biochem Sci* 2015) |
| `doi-lookup.json` | `esearch.fcgi?db=pubmed&retmode=json&retmax=1&term="10.1126/science.1063991"[AID]` — how `fetch doi:…` finds the PMID |
| `fetch.json` | `esummary.fcgi?db=pubmed&retmode=json&id=11711636` — Abel, Bryan & Norman 2002, not in PMC (one location) |
| `fetch-pmc.json` | `esummary.fcgi?db=pubmed&retmode=json&id=25497837` — Lin et al. 2014 (*eLife*), has a `pmc` article id and `pmcrefcount` 26 (two locations) |
| `cites.json` | `elink.fcgi?dbfrom=pubmed&db=pubmed&linkname=pubmed_pubmed_refs&retmode=json&id=25497837` — 26 references |
| `cites-empty.json` | the same for `id=11711636`: an old paper with no linked references, a `linkset` with no `linksetdbs` |
| `cited-by.json` | `elink.fcgi?dbfrom=pubmed&db=pubmed&linkname=pubmed_pubmed_citedin&retmode=json&id=11711636` — 13 PMC-indexed citers |
| `not-found.json` | `esummary.fcgi?db=pubmed&retmode=json&id=99999999999` — the `"error": "cannot get document summary"` stub |
| `empty-search.json` | `esearch.fcgi?db=pubmed&retmode=json&retmax=3&term=zzqxjk[TI] AND qqvwxz[AU]` — `count` 0, `idlist` empty, `errorlist` |

No 429 fixture: the two rate-limit tests (`a_429_backs_off_and_is_never_retried`
and `eutils_rate_limit_error_in_a_200_is_a_rate_limit_too`) use **hand-written**
bodies in `src/tests.rs`, the documented E-utilities shape
(`{"error":"API rate limit exceeded",…}`); NCBI does not hand one out on request.

## Facts the fixtures do not show

- **Author queries are unquoted.** `"Doudna"[Author] AND CRISPR[Title]` finds
  nothing (0 hits); `Doudna[Author] AND CRISPR[Title]` finds 149, and the
  adapter writes the second form. Found by the live test, 2026-10-19.
- **Rate limit.** NCBI answers 429 to a burst within one second even when the
  process stays under 3 per second on average; a token bucket of 3 failed
  intermittently, one token refilled every `1.4 s / n` (≈2 req/s anonymous,
  ≈7 with a key) did not. The `x-ratelimit-limit` header (3, or 10 with a key)
  sets `n`.
- **esummary carries no abstract**, and gives author initials, not given
  names. The adapter does not call `efetch` (XML) to fill them; `abstract`
  is therefore not in the explain record's `fills`.
- **`pmcrefcount`** is the number of PubMed Central articles citing the
  record, not a total citation count; `metrics.cited_by.source` says
  `pubmed-pmc`.
- **PMC location version.** esummary does not say which version PMC holds;
  the second location is labelled `accepted` (author manuscript is the
  commonest deposit) — a stated assumption, not a fact read from the record.
