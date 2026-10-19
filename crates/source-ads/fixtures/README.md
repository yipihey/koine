# Recorded ADS / SciX responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against `https://api.adsabs.harvard.edu/v1` with a real
API token, read from the macOS keychain item the imbib app stores
(`security find-generic-password -a com.imbib.credentials.ads.apiKey -w`) into
a shell variable and sent as `Authorization: Bearer …`; it was never printed,
and no recorded file contains it (`grep` for `Bearer` finds nothing; the
`set-cookie` response header was deleted from the `.headers` files, being a
session cookie with no bearing on the adapter). Request shape:

    curl -sS -A "koine-source-ads/0.1 (mailto:tabel@stanford.edu)" \
      -H "Authorization: Bearer $T" -D <name>.headers -o <name>.json \
      -G https://api.adsabs.harvard.edu/v1/search/query \
      --data-urlencode "fl=<parse::FIELDS>" --data-urlencode "q=…" …

Every fixture is a real response; none is hand-written. The Tier A tests in
`src/tests.rs` serve them from a mock server with a fixed fake token; nothing
in `cargo test` reaches ADS. The queries below are exactly what
`query::map` compiles from the canonical queries the tests send.

| File | Request |
|---|---|
| `search.json` / `.headers` | `q=(author:"Abel" AND title:"first star" AND year:2002)`, `rows=3`, `start=0`, `sort=score desc`: `numFound` 2 (the Science paper and a conference paper) |
| `search-more.json` / `.headers` | `q=(author:"Abel, T" AND year:[2000 TO 2003])`, `rows=2`, `start=0`: `numFound` 52, for offset paging |
| `fetch.json` / `.headers` | `q=bibcode:"2002Sci...295...93A"`, `rows=1`: a refereed article with `PUB_HTML`, `EPRINT_HTML`, `EPRINT_PDF` esources |
| `fetch-eprint.json` / `.headers` | `q=identifier:"arXiv:1706.03762"`, `rows=1`: an `eprint` doctype (`2017arXiv170603762V`), arXiv's own DOI, `page: ["arXiv:1706.03762"]` |
| `fetch-miss.json` / `.headers` | `q=bibcode:"1900Zzz...999..999Z"`, `rows=1`: `numFound` 0 |
| `references.json` / `.headers` | `q=references(bibcode:"2002Sci...295...93A")`, `rows=2` (`numFound` 40) |
| `citations.json` / `.headers` | `q=citations(bibcode:"2002Sci...295...93A")`, `rows=2` (`numFound` 1469) |
| `similar.json` / `.headers` | `q=similar(bibcode:"2002Sci...295...93A")`, `rows=2` |
| `coreads.json` / `.headers` | `q=trending(bibcode:"2002Sci...295...93A")`, `rows=2` — the operator scix-client's `coreads` uses |
| `export-bibtex.json` / `.headers` | `POST /export/bibtex` with `{"bibcode":["2002Sci...295...93A"]}` |
| `export-ris.json` / `.headers` | `POST /export/ris`, same body |
| `unauthorized.json` / `.headers` | the same search with `Authorization: Bearer invalid`: HTTP 401 |
| `syntax-error.json` / `.headers` | `q=author:(`, `rows=1`: HTTP 400 with the Solr parse error under `error.msg` |

## Facts the fixtures show

- Quota is per day: `x-ratelimit-limit: 5000`, `x-ratelimit-remaining`, and
  `x-ratelimit-reset` as an **epoch** in seconds. (A `retry-after: 86400` also
  rides on 200 responses; the adapter reads it only on a 429.)
- `doi` is a list and, for papers on arXiv, includes arXiv's own DOI
  (`10.48550/arXiv.…`); the work's `ids.doi` is the publisher's.
- `page` is `["93"]` and `page_range` is `"93-98"` where ADS has it; an
  e-print's `page` is its own id.
- `orcid_pub` is parallel to `author`, with `-` for "none".
- `esources`: `PUB_HTML`, `PUB_PDF`, `EPRINT_HTML`, `EPRINT_PDF`, `ADS_PDF`,
  `ADS_SCAN`, `AUTHOR_*`; `property` carries `EPRINT_OPENACCESS`,
  `PUB_OPENACCESS`, `OPENACCESS`, `REFEREED`.
