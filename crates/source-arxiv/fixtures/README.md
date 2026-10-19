# Recorded arXiv responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against `https://export.arxiv.org/api/query` (Atom 1.0
with the `opensearch` and `arxiv` namespaces) with
`curl -sS -A "koine-source-arxiv/0.1 (mailto:tabel@stanford.edu)" -D <name>.headers -o <name>.xml -G <url> --data-urlencode …`,
one request at a time with `sleep 4` between them (arXiv's terms: one request
per three seconds). Nothing is redacted: the API takes no credential. The
Tier A tests in `src/tests.rs` serve these from a mock server; nothing in
`cargo test` reaches arxiv.org.

| File | Request (parameters of `GET /api/query`) |
|---|---|
| `search.xml` / `search.headers` | `search_query=au:Abel AND ti:"first star"`, `start=0`, `max_results=3`, `sortBy=relevance`, `sortOrder=descending`: 3 entries of `totalResults` 5; one entry has `arxiv:doi`, entities (`&gt;`) in a summary |
| `fetch.xml` / `fetch.headers` | `id_list=astro-ph/0112088`, `max_results=1`: a legacy-id entry with `arxiv:doi` |
| `fetch-new.xml` / `fetch-new.headers` | `id_list=1706.03762v7`, `max_results=1`: a modern id at version 7, eight authors, no DOI |
| `empty.xml` / `empty.headers` | `id_list=9999.99999`, `max_results=1`: `totalResults` 0 and no entry (the miss) |
| `error.xml` / `error.headers` | `id_list=nonsense`, `max_results=1`: arXiv answers HTTP 200 with one `<entry>` whose id is `…/api/errors#…` and title `Error`; the adapter reads that as an `error` diagnostic |

The 429 path has no fixture: arXiv does not hand one out on request, and the
test builds the response (`retry-after: 3`) in the mock itself.

## Facts the fixtures show

- arXiv sends no rate-limit headers; the adapter's limiter is seeded with the
  terms-of-use rate (1 request per 3 s) rather than read back.
- An id lookup (`id_list`) combined with `search_query` is an intersection;
  the adapter relies on that to honour a `bib.arxiv =` clause beside others.
- The `<id>` of an entry carries the latest version (`…v1`, `…v7`); the work's
  `ids.arxiv` is the versionless id and the location's `source_id` is the
  versioned one.
