# Recorded Crossref responses (S-D1: the conformance fixture per adapter)

Recorded 2026-10-19 against Crossref REST API `message-version` 1.0.0 with
`curl -A "koine-source-crossref/0.1 (mailto:tabel@stanford.edu)"` and
`mailto=tabel@stanford.edu` (the polite pool; the `x-api-pool: polite-array`
header in each `.headers` file is the proof). The Tier A tests in
`src/tests.rs` serve these from a mock server; nothing in `cargo test`
reaches api.crossref.org.

| File | Request |
|---|---|
| `search.json` / `search.headers` | `GET /works?query.bibliographic=reionization 21cm&query.author=Abel&rows=3&cursor=*` — a `work-list` with `next-cursor` and the `x-rate-limit-*` headers |
| `filter.json` / `filter.headers` | `GET /works?query.author=Abel&filter=from-pub-date:2002-01-01,until-pub-date:2002-12-31&rows=2` — the `year within` mapping |
| `fetch.json` / `fetch.headers` | `GET /works/10.1126/science.1063991` — a single `work` (Abel, Bryan & Norman 2002) |
| `export.bib` / `export-bibtex.headers` | `GET /works/10.1126/science.1063991/transform` with `Accept: application/x-bibtex` |
| `rate-limited-429.json` / `.headers` | **Hand-written**, not recorded: Crossref does not hand out a 429 on request. The headers are the documented shape (`retry-after`, `x-rate-limit-limit`, `x-rate-limit-interval`); the body is a plausible `status: error` document. |
