# Recorded OpenAlex responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against `https://api.openalex.org` with
`curl -sS -A "koine-source-openalex/0.1 (mailto:tabel@stanford.edu)" -D <name>.headers -o <name>.json …`
and `mailto=tabel@stanford.edu` (the polite pool). Nothing is redacted: the
API was called without a key. The Tier A tests in `src/tests.rs` serve these
from a mock server; nothing in `cargo test` reaches api.openalex.org.

| File | Request |
|---|---|
| `search.json` / `search.headers` | `GET /works?filter=raw_author_name.search:Abel,title.search:first star&per-page=3&cursor=*&sort=relevance_score:desc` — 3 of `meta.count` 11, with `meta.next_cursor` |
| `fetch.json` / `fetch.headers` | `GET /works/https://doi.org/10.1126/science.1063991` — one full `Work`: five `locations`, `best_oa_location`, `abstract_inverted_index`, `biblio` |
| `cites.json` / `cites.headers` | `GET /works?filter=cites:W2040912209&per-page=2&cursor=*&sort=cited_by_count:desc` — the works that cite it (`meta.count` 1448) |
| `cited-by.json` / `cited-by.headers` | `GET /works?filter=cited_by:W2040912209&per-page=2&cursor=*&sort=cited_by_count:desc` — the works it cites (`meta.count` 47) |
| `notfound.json` / `notfound.headers` | `GET /works/W1` — a 404 whose body is an HTML page, not JSON |
| `budget-exhausted-429.json` / `.headers` | **Hand-written**, not recorded: a plausible 429 with `retry-after: 7`, `x-ratelimit-remaining: 0`, `x-ratelimit-reset: 3600` (the documented headers); OpenAlex was not driven into its limit. |

## Facts the fixtures show

- The recorded `search.headers` carry `x-ratelimit-limit: 1000`,
  `x-ratelimit-remaining`, `x-ratelimit-reset` (seconds): since 2026 OpenAlex
  meters a **daily budget** per IP (or per `api_key`), and the response `meta`
  carries a `notice` saying so; `mailto` does not raise it. The adapter backs
  the source off until the reset when `remaining` reaches 0.
- Filters that did **not** exist on `/works` when probed (all answered
  "Invalid query parameters"): `primary_location.source.display_name[.search]`,
  `locations.source.display_name`, `host_venue.display_name`,
  `cites:doi:…` / `cites:https://doi.org/…` (the `cites`, `cited_by` and
  `related_to` filters take only `W…` ids). Hence no `container` index and
  `bib.cites`/`bib.cited_by` need a work id.
- `title.search` and `abstract.search` normalise server-side to
  `display_name.search` / `abstract.search`; `cites:W` to `referenced_works:W`.
