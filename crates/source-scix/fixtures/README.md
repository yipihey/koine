# SciX fixtures (S-D1: the conformance fixture per adapter)

**Everything here is hand-written except `unauthorized-401.*`.** No SciX token
was reachable on the machine this was written on (imbib keeps it in a keychain
access group another process cannot read; no `KOINE_SECRET_SCIX_TOKEN` was
set). The shape is the documented ADS/SciX `search/query` response
(`responseHeader`, `response{numFound,start,docs[]}`, with `bibcode`, `title[]`,
`author[]`, `orcid_pub[]`, `year`, `pubdate`, `pub`, `volume`, `issue`, `page[]`,
`abstract`, `doi[]`, `identifier[]`, `citation_count`, `esources[]`,
`property[]`, `doctype`, `lang[]`) and the `{"error": {...}}` body of a bad
query.

| File | Stands for / provenance |
|---|---|
| `unauthorized-401.json` / `.headers` | **Recorded** 2026-10-19: `curl -A "koine-source-scix/0.1 (mailto:tabel@stanford.edu)" 'https://api.scixplorer.org/v1/search/query?q=bibcode%3A2002Sci...295...93A&fl=bibcode&rows=1&start=0'` with no `Authorization` header: HTTP 401, `{"message": "Missing \"Authorization\" in headers."}`. This also shows `api.scixplorer.org/v1` is a live ADS-compatible host (`scixplorer.org/api/v1` is a 404 web page). |
| `search.json` / `.headers` | a `q=(author:"Abel, T" AND title:(first AND star))&rows=3` page: three docs, `numFound` 13 |
| `fetch.json` / `.headers` | one doc for `bibcode:2002Sci...295...93A` |
| `citations.json` / `.headers` | a `citations(bibcode:…)` page: two docs, `numFound` 5 |
| `empty-search.json` / `.headers` | no hits |
| `rate-limited-429.json` / `.headers` | a 429 with `retry-after: 2` and `x-ratelimit-remaining: 0` |
| `bad-syntax-400.json` | the `error.msg` body of an unparsable query |

How far to trust them: Abel, Bryan & Norman 2002 carries its real bibcode, DOI,
arXiv id (`astro-ph/0112088`), pages and the citation count SciX reported on
2026-10-19 (1469, read through the SciX MCP tool). The other two documents are
illustrative, `esources` and `property` lists are typical rather than read, and
the `x-ratelimit-*` values and the `reset` epoch are invented (header names and
the daily 5000 budget are the ADS convention, and imbib's Swift
`SciXSource.metadata` states 5000 per 86400 s).

When a token is available: re-record each with
`curl -H "Authorization: Bearer $TOKEN" -D x.headers -o x.json '<url>'` (never
print or commit the token), correct the table, and run
`KOINE_SECRET_SCIX_TOKEN=… cargo test -p source-scix -- --ignored live`.

## What the Swift did that this does not

- `SciXSource.swift` called the **ADS** host (`api.adsabs.harvard.edu/v1`,
  through `scix-client`) with a SciX token and rewrote the links; this adapter
  calls `api.scixplorer.org/v1`.
- It exported BibTeX and RIS through `scix-client`'s export calls; here
  `export` is `csl-json` only, from the parsed record.
- A shared ADS-family base (query mapping, parser, rate headers) with the
  parallel `source-ads` crate is a follow-up once both land; the two are
  deliberately not coupled now.
