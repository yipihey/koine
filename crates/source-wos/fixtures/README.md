# Web of Science Starter API fixtures (S-D1: the conformance fixture per adapter)

**Every file here is hand-written, none is recorded.** The Starter API needs a
Clarivate developer-portal key and none was reachable on the machine this was
written on (imbib keeps it in its keychain access group, which another
process cannot read; no `KOINE_SECRET_WOS_API_KEY` was set). The shapes are the
documented `GET /documents` response of the Starter API
(`metadata{total,page,limit}`, `hits[]` with `uid`, `title`, `types`,
`source{sourceTitle,publishYear,publishMonth,volume,issue,pages{range}}`,
`names.authors[]{displayName,wosStandard,researcherId}`, `links`,
`citations[]{db,count}`, `identifiers{doi,issn,pmid}`,
`keywords.authorKeywords`) and the field names the Swift
`WoSTypes.swift` decodes.

What that means for how far to trust them:

- The **UTs are synthetic** (`WOS:000000000000001`…`003`), the citation count
  (1000) is invented, and the second and third hits only borrow plausible
  titles and DOIs. Abel, Bryan & Norman 2002 (`fetch.json`) carries its real
  title, authors, journal, volume, issue, pages and DOI.
- The rate-limit headers (`x-ratelimit-limit-second`,
  `x-ratelimit-remaining-second`, `x-ratelimit-limit-year`,
  `x-ratelimit-remaining-year`, and `retry-after` on the 429) are the
  gateway's documented names, not captured ones. If a real response names
  them differently the bucket simply keeps its 5 req/s seed.
- The field-tag grammar in `src/query.rs` (`AU=(Abel T)`, `PY=2020-2024`,
  `AK=("dark matter")`) is the documented advanced-search syntax; `PY=1000-1999`
  for `year <` and the `AK` tag are the two least certain spellings.

| File | Stands for |
|---|---|
| `search.json` / `search.headers` | `GET /documents?q=(AU=(Abel T) AND TI=(first AND star))&db=WOS&limit=3&page=1&sortField=RS+D` — three hits, `total` 13 |
| `fetch.json` / `fetch.headers` | `GET /documents/WOS:000000000000001` — one document |
| `doi-lookup.json` / `.headers` | `GET /documents?q=DO=(10.1126/science.1063991)&db=WOS&limit=1&page=1` |
| `empty-search.json` / `.headers` | a search with no hits |
| `rate-limited-429.json` / `.headers` | a 429 with `retry-after: 2` |
| `unauthorized-401.json` | a 401 for a bad key (becomes the `refused` diagnostic) |
| `not-found-404.json` | a 404 on `/documents/{uid}` |

When a key is available: re-record each with
`curl -H "X-ApiKey: $KEY" -A "koine-source-wos/0.1 (mailto:…)" -D x.headers -o x.json '<url>'`
(never print or commit the key), replace these files, correct the table above
and run `KOINE_SECRET_WOS_API_KEY=… cargo test -p source-wos -- --ignored live`.

## What the Swift did that this does not

The Swift adapter spoke the **Expanded** API (`wos-api.clarivate.com/api/wos`,
`usrQuery`, `count`, `firstRecord`, `/citing`, `/references`, `/related`) and
generated BibTeX/RIS from the record itself. The Starter API has no
per-record citing/references/related endpoints, so `related` answers
`unsupported`; export is `csl-json` only, from the parsed record.
