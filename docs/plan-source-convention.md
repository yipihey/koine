# The source convention: how an online database declares itself

**Status:** design, 2026-10-19. Nothing is built. **Builds on:**
plan-run-convention.md (the first domain convention; this is the second, and
the two are written to the same pattern so the common part can be lifted
once both exist), plan-koine-trust.md (TR-D12 kinds, TR-D13 keeping the
unknown, TR-D4 review), plan-koine-followups.md (F-D1 secrets, F-D2 rows),
plan-koine-host.md (H-D8 `returns_kind`), plan-koine-ai-tasks.md (jobs,
`emit`), plan-koine-live.md (L-D5 the `{rows, next}` envelope).
**Specialises for:** imbib, whose seven sources, canonical record and
resolution cascade are the worked example, and which gains a search verb
it does not have today.

## Why

imbib talks to seven online databases (arXiv, ADS, SciX, Crossref, PubMed,
OpenAlex, Web of Science) and was surveyed on 2026-10-19. The findings are
the simulation findings again, in a second domain:

| Gap | What imbib does today |
|---|---|
| No declaration of what a source can do | the Swift `SourcePlugin` protocol has `search(string)` and `fetchBibTeX`; fetch-by-id, citations, references, PDF links and related works are per-source extra methods every caller has to know about |
| No source-independent query | every source gets the same string and parses it its own way (arXiv maps `au:`/`ti:`, OpenAlex has its own parser, ADS passes Lucene through, Crossref and PubMed take it raw); fetch-by-id is faked as a search in source-native syntax |
| Provenance is dropped | a result becomes a store row through *synthesized* BibTeX; the payload's `source_id` exists in the schema and nothing fills it; `produced_by` is `None`. Nothing records which database a paper came from |
| Three implementations, none complete | the live GUI path is Swift-only; `crates/impress-sources` (Rust, four sources live, three `unimplemented!`) serves only the enrichment daemon; `imbib-core/src/sources` holds dead parsers. ADS is implemented twice. Semantic Scholar and DBLP exist only in enums; `apps/imbib/CLAUDE.md` says they have plugins |
| Dedup disagrees with itself | two source-priority tables that rank the sources differently (Swift: WoS first; Rust: Crossref first, SciX and WoS absent), three dedup paths |
| No search verb | `search_sources` and `resolve_citation` forward to the running Swift app and refuse when it is closed; the resolution cascade and its scorer — the expert knowledge — live only in `AutomationService.swift` |
| Rate limits and etiquette are ad hoc | one `RateLimiter` actor per source, 429 retry in one caller, the polite-pool email hardcoded per file, nothing in Rust |

And the external reason, which the run convention does not have: **agents
fabricate citations.** The 2026 evaluations (HALLMARK; Rao & Callison-Burch;
GhostCite) find 14–95% hallucinated citations by model in free generation;
search-enabled frontier models reach 51% fully-correct entries; a
deterministic, id-based retrieval tool in a search-then-verify loop reaches
78%, with a tenth of the regressions. The single most useful thing koine can
give an agent writing a paper is a search that returns identifiers and a
resolve that scores its match. That tool does not exist today.

### What the prior art settled

- **SRU/ZeeRex "explain"** is the parameter schema's twin: a source
  declares its searchable indexes, the relations each supports, sortable
  fields, maximum records and record schemas, so that a client can
  "self-configure and build a suitable user interface". OpenSearch does the
  same with a URL template. GraphQL introspection and OpenAPI declare the
  *shape* of a response and not which fields are searchable, which leaves an
  agent guessing.
- **CQL** is the query grammar: `index relation term` with named *context
  sets* (`dc.title any "dark matter"`), and a server that lacks an index
  answers a **diagnostic**, never a silent fallback. Z39.50 faded because an
  author search hit an author index on one server, personal names on a
  second, keywords on a third, and an error on a fourth, so "results for the
  same query can vary widely"; SRU over HTTP replaced it and kept the
  diagnostics.
- **Zotero translators** are the adapter precedent at scale (hundreds of
  sources): a header declares type, target and priority; `detectWeb` says
  whether the translator applies; `doWeb` does the work; "add by identifier"
  fans one identifier out to every search translator.
- **CSL-JSON** is the canonical record the citation world already agreed on
  (`additionalProperties: false` plus a free `custom` object), and
  **OpenAlex** shows how to merge: one work with an `ids{}` map holding every
  source's identifier, `locations[]` each with a DRIVER `version`
  (submitted, accepted, published), so a preprint DOI resolves to the merged
  work and the version is a property of a location, never a second record.
- **Crossref's search-based matching with validation** (SBMV): send the raw
  reference string, normalise relevance by length, then validate year,
  volume, pages and first author; F1 0.845 against 0.529 for a parser.
  ADS's resolver and Semantic Scholar's `search/match` do the same and
  return a score. imbib's `scoreCandidate` is a hand-rolled SBMV.
- **Etiquette is protocol:** Crossref's polite pool wants a `mailto`, ADS a
  bearer with a per-endpoint daily quota in `X-RateLimit-*` headers,
  Semantic Scholar one request per second per key, Unpaywall an email.
  habanero reads the limits from the headers rather than hard-coding them;
  Crossref says it has blocked careless callers.
- **Pagination is three models**, not one: capped offset (ADS `start`,
  OpenAlex to 10,000), opaque cursor (Crossref `cursor=*`, OAI-PMH
  `resumptionToken`), continuation token (Semantic Scholar bulk). A client
  treats all three as opaque.
- **Agent MCP servers exist for ADS, Semantic Scholar and arXiv**, each
  community-written, each with its own tool names, the arXiv one bounding
  results at 5 by default and 50 at most. None is generated from a
  declaration, so three sources are three vocabularies.
- **Aggregators without a protocol die with their host**: Papers with Code
  shut down in July 2025; `scholarly` scrapes Google Scholar through a
  carousel of proxies.

## Decisions

### S-D1. A source is an adapter that emits an explain record

A source is one online database reachable by one adapter. The adapter
emits, on request (`Source.explain()`), a JSON document that says what the
source can do — nothing more than the facts, and all of them:

```json
{
  "source": "crossref", "name": "Crossref", "version": "rest-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match"],
  "indexes": {
    "any":      {"relations": ["any"]},
    "title":    {"relations": ["any", "adj"]},
    "author":   {"relations": ["any"]},
    "doi":      {"relations": ["="], "exact": true},
    "year":     {"relations": ["=", "<", ">", "within"]}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["doi"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "cited_by", "license"],
  "page": {"model": "cursor", "max": 1000},
  "etiquette": {"contact": "mailto", "limit": "headers", "key": "optional"},
  "secrets": ["crossref-plus-token"]
}
```

| Field | What it declares | Precedent |
|---|---|---|
| `operations` | which of the fixed verb set (S-D3) this source answers | Zotero `translatorType` |
| `indexes` | the context-set indexes (S-D2) it can search, and the relations each supports; `exact` marks an identifier lookup | ZeeRex `indexInfo` |
| `sortable` | fields a result list can be ordered by | ZeeRex `sort` |
| `identifiers` | which of the canonical id kinds (S-D4) it can look up directly | Semantic Scholar's id grammar |
| `fills` | which canonical fields this source populates, so a client shows *absent* against *unknown* | ADS `fl`, S2 `fields` |
| `page` | `offset` (with a cap), `cursor` or `token`; the client treats all as opaque | the three models observed |
| `etiquette` | how to be a good caller: contact header, where limits are read, whether a key is optional | Crossref polite pool, habanero |
| `secrets` | the F-D1 secret names a call needs; never an argument | plan-koine-followups F-D1 |

An adapter's explain record names the **source API version** it was
written against, and a conformance fixture per adapter replays a recorded
response of that version. The port will meet the reason at once: the Rust
Crossref adapter sends `query.title`, which Crossref has deprecated for
`query.bibliographic`; a field table in code drifts silently, a declared
one has a version to check. An adapter that answers a query its explain
record does not cover is a conformance failure. A frontend builds the search form from `indexes`, the
sort menu from `sortable`, the "look up by" field from `identifiers`, and
greys out nothing by guessing: this is ZeeRex's stated purpose and the
parameter schema's twin (plan-run-convention R-D2).

### S-D2. One query grammar, with diagnostics instead of fallbacks

A query is CQL's shape, carried as JSON so it needs no parser on either
side:

```json
{"and": [
  {"index": "author", "relation": "any", "term": "Abel"},
  {"index": "year",   "relation": "within", "term": "2020 2024"},
  {"or": [{"index": "title", "relation": "any", "term": "reionization"},
          {"index": "any",   "relation": "any", "term": "21cm"}]}
]}
```

The indexes are a **context set** this convention owns, `bib.*`: `any`,
`title`, `abstract`, `author`, `first_author`, `year`, `container`
(journal, venue, bibstem), `doi`, `arxiv`, `pmid`, `bibcode`, `isbn`,
`issn`, `orcid`, `keyword`, `subject`, `cites`, `cited_by`, `similar_to`.
The three relation indexes read from the result's side: `cites = X` is
"works that cite X", `cited_by = X` is "works in X's reference list",
`similar_to = X` is "works like X". The `related` verb's `relation` reads
the same way. Settled when S3 found the plan silent and ADS, OpenAlex and
Crossref each name the direction differently.
An adapter maps each to its native syntax (`author` → ADS `author:"…"`,
arXiv `au:`, Crossref `query.author`, OpenAlex `filter=authorships.author…`)
or, for an index its explain record does not list, answers a **diagnostic**
for that clause — `{"unsupported": "bib.first_author"}` — and the clause is
dropped *visibly*, in the result's `diagnostics`. Z39.50's silent
author→keyword fallback, the failure that made "the same query vary
widely", is what this forbids.

A source may additionally accept its **native query** as a passthrough,
`{"native": "author:\"Abel, T\" year:2020-2024"}`, declared in the explain
record as `native: "ads-lucene"`. imbib's smart search (`impress-smart-search`),
which classifies pasted text and builds ADS Lucene, keeps producing that;
the convention gives it a place rather than a second grammar. Every
canonical-grammar query is translatable to a native one; the reverse is not
promised.

### S-D3. The verbs are fixed, generic, and generated once

One `source-service` over every adapter, not a service per source:

| Verb | Arguments | Answer | Safety |
|---|---|---|---|
| `source-service_explain` | `source?` | the explain records (all, or one) | read-only |
| `source-service_search` | `query` (S-D2), `sources?` (default: those whose explain covers the query), `limit` (default 10, max 100 per source), `cursor?`, `sort?` | `{rows, next, diagnostics}` — the L-D5 envelope; each row a canonical record (S-D4) with its `source` and `source_id` | read-only, external |
| `source-service_fetch` | `id` (any canonical identifier, S-D4), `sources?` | the merged record, or `not-found` | read-only, external |
| `source-service_match` | `citation` (a free string, or the structured fields of a reference), `sources?` | candidates each with a `score` and `evidence`, the best marked when it clears the threshold | read-only, external |
| `source-service_related` | `id`, `relation` (`cites`, `cited_by`, `similar`, `coreads`), `limit`, `cursor?` | `{rows, next}` | read-only, external |
| `source-service_export` | `ids`, `format` (`bibtex`, `ris`, `csl-json`) | the text | read-only, external |

Every verb is `external` (it leaves the process, ADR-0036's class) and
`read-only` on the store: a search writes nothing. **Importing** a result
into a library is imbib's own `imbib-library-service_import-*`, which takes a
canonical record and is where the row is made — with its provenance (S-D5).
The separation is the one TR-D7 wants for review: searching is free,
importing a hundred papers is what a person confirms.

From one `#[impress_service]` these are the MCP tools, the CLI, the App
Intents and the generated forms (impress-verb-surface reads `indexes` from
the explain record to build the search form, the way it reads a parameter
schema). The three community MCP servers for ADS, Semantic Scholar and
arXiv are three vocabularies for one thing; this is one vocabulary for all
sources, with results bounded by default (`limit` 10) and every row
carrying identifiers, which is what the citation-faithfulness evaluations
found decisive.

### S-D4. The canonical record is CSL-JSON with an ids map and locations

The kind `bibliographic-work@1`:

- **Body**: CSL-JSON (`type`, `title`, `author[]` as `{family, given,
  orcid?}`, `issued` as date-parts, `container-title`, `volume`, `issue`,
  `page`, `abstract`, `language`), because the citation world has already
  agreed on it and every style processor reads it. CSL's `custom` object is
  where the rest goes, under this convention's keys.
- **`ids`**: a map of every identifier any source supplied — `doi`, `arxiv`,
  `pmid`, `pmcid`, `bibcode`, `isbn`, `openalex`, `s2`, `mag`, `wos`, `dblp`
  — never one chosen id. OpenAlex and Semantic Scholar both converged on
  this; imbib's `Identifiers` struct already is one.
- **`locations[]`**: each `{source, source_id, version, url, pdf?, license?,
  oa_status?}` with DRIVER's `version` (`submitted`, `accepted`, `published`),
  and a `primary` flag. A preprint and its published version are **one
  record with two locations**, never two records; a preprint DOI resolves to
  the merged record. This is the failure mode the resolver literature names
  first (PreprintResolver), and imbib's inbox feeds live in it: an arXiv
  feed and a journal feed of the same paper are one work.
- **`provenance`**: `{fetched_at, source, source_id, query?}` per fetch — the
  field imbib's import drops today.
- **`metrics`**: `cited_by`, with its source, since counts differ by source.

`imbib/bibliography-entry` stays what it is — the BibTeX-shaped kind
imbib's library is made of — and the import verb maps a work into it. The
work is the wire and the merge shape; the entry is the library row. Two
kinds, one direction, one mapping, which is the discipline `schema-refs.json`
already enforces.

### S-D5. Provenance is kept at import, and never synthesized away

`imbib-library-service_import-work` (new) takes a `bibliographic-work@1`
and writes the `imbib/bibliography-entry` row with: `source_id` filled
(the payload field that exists and nothing fills), `produced_by` set to the
search call's record when one exists, the source's **own** BibTeX kept as
`raw_bibtex` when the source supplied one (today's `toBibTeX()` synthesises
it from fields and loses the source's text), and the `ids` map copied in
full. The existing `batch_import_search_results` funnel is retired in
favour of this one.

### S-D6. Resolution is one scored cascade, in Rust, with its scorer declared

`source-service_match` is `AutomationService.resolveStructuredCitation`
moved into Rust and made honest:

1. **Identifier first.** Any canonical id in the input (or scanned from its
   text by `im-identifiers`) → `fetch`. A hit is `via: "identifier"`,
   score 1.
2. **Local store.** The library's own rows, by identifier then by the
   scorer; `via: "local"`.
3. **Search-based matching with validation** (Crossref's SBMV, which is
   what imbib's cascade already approximates): a structured query to the
   sources whose explain record lists `match` or the needed indexes, then
   the scorer over the candidates. The scorer is today's `scoreCandidate`
   (first author 0.25, year 0.20, container 0.20, title overlap to 0.20,
   identifier 0.15) and today's acceptance rules (top ≥ 0.85 with margin
   ≥ 0.15; single hit ≥ 0.50; top ≥ 0.70 with margin ≥ 0.25), **declared in
   the answer**: each candidate carries its `score` and the `evidence` that
   produced it (`{first_author: 0.25, year: 0.20, …}`), and the answer says
   whether the threshold was met and by which rule.
4. **Nothing auto-merges below threshold.** Below it, candidates are
   returned for a person or an agent to choose; the choice is a call with
   the chosen id, logged.

The weights and thresholds are constants in one Rust module with a Tier A
golden suite, and the two acceptance tables that disagree today
(`deduplicationPriority` in Swift, `SOURCE_PRIORITY` in Rust) become one,
in the same module.

`scoreCandidate` has no tests today (checked 2026-10-19). The golden suite
is therefore **recorded** from the Swift scorer before the port, over a
fixture set of real references with known answers, and it pins
*behaviour*, not correctness: a golden that fails after a deliberate
scorer change may itself be wrong, and is then verified against the source
and edited in place (the lesson of the smart-search port, where goldens
captured from Swift encoded its bugs). The weights are a decision to
revisit once the suite exists, not before.

### S-D7. Federation streams, merges by identifier, and never blocks on one source

`search` with several sources runs them concurrently as a job (plan-koine-
ai-tasks: a long-running verb with `emit`): each source's rows are emitted
as they arrive with the source named, a per-source timeout turns a slow
source into a diagnostic (`{"source": "wos", "status": "timed-out"}`), and
the merge is **by identifier only** — `ids` overlap on `doi`, `arxiv`,
`pmid`, `bibcode` — into one record with several locations. Cross-source
relevance scores are not fused (they are not comparable; the metasearch
literature's standing finding); the merged list is ordered by the requested
`sort`, or by the first source's order when `relevance` was asked.
Title-only fuzzy merging is off, as imbib's is today, and a `match` call is
how a person asks for it.

The envelope's `diagnostics` names every source's outcome — `done`,
`refused` (no secret), `timed-out`, `unsupported` (an index it lacks),
`rate-limited` — so a result that is missing a source says so, which is
TR-D7's "done" against "nobody was listening", for databases.

### S-D8. Etiquette is the adapter's and is enforced once

Every adapter declares its etiquette (S-D1) and the service enforces it in
one place: a per-source token bucket seeded from the declaration and
*refilled from response headers* when the source sends them (Crossref's
`X-Rate-Limit-*`, ADS's `X-RateLimit-Remaining`); the contact string
(`mailto`, User-Agent) set once from the host's identity, never per file;
a 429 answered by backing off that source and emitting `rate-limited`, not
by retrying in the caller; a secret read through F-D1's slot, never passed.
The daily budgets (ADS 5,000 per endpoint, Unpaywall 100,000) are in the
declaration and the remaining count is a `source-service_explain` field, so
a person can see that today's ADS quota is nearly spent before a sweep
spends it.

### S-D9. Adapters are packages

An adapter is a koine package: its explain record, its query mapping, its
response parser into `bibliographic-work@1`, its etiquette, its Tier A
fixtures (recorded responses), and a `detect` rule — Zotero's `detectWeb`:
a URL or an identifier pattern the adapter claims (`arxiv.org/abs/…`,
`10.\d+/…`), so "add from this URL" fans out to the adapters that claim it.
Installing a package adds a source to every host that links
`source-service`; `source-service_explain` is the inventory. The seven
imbib sources are the first seven packages, ported from
`crates/impress-sources` (four live) and the Swift plugins (three more);
Semantic Scholar, DBLP and INSPIRE are the next three and need no change
anywhere else. The `SourceID` enum, closed today, goes.

### S-D10. imbib's specialisation, exactly

What imbib keeps and what it gains:

- **Keeps**: smart search (classification, ADS query building) as the
  producer of `native` queries; the inbox, feeds and retention, now over
  `search` with a saved query (a `bib.*` query is JSON, so a smart search
  row stores the query itself instead of `query` plus `source_ids_json`);
  dismissal over `ids`; the SciX library verbs, which are SciX's own
  surface and stay a separate service.
- **Gains**: a search verb that works with the app closed (every adapter is
  Rust; the Swift `SourcePlugin` protocol and its seven implementations are
  deleted once the ported adapters pass their fixtures, as the TypeScript
  MCP server was); provenance on every imported row; one resolution cascade
  callable from the CLI, MCP and the citation palette alike, with its scores
  shown; one dedup table; one etiquette layer; and the agent-facing tool the
  hallucination studies ask for.
- **Specialises**: the `bib.*` context set is bibliographic by design. A
  second domain (materials, protein structures, time-series catalogues)
  declares its own context set and canonical kind over the same
  `source-service` verbs; `explain.kind` says which. That is the general
  part, and it is small: S-D1, S-D2's shape, S-D3, S-D7, S-D8, S-D9 are
  domain-free; S-D4, S-D5, S-D6 and S-D10 are imbib's.

### S-D11. Not in this plan

- Full-text search inside PDFs (imbib's `full_text_search` is local and
  stays).
- Library sync with a source (SciX libraries, Zotero): a different verb
  set, over the same adapters later.
- Harvesting (OAI-PMH `ListRecords` for building a local mirror): a `harvest`
  operation an adapter may declare, specified when a source needs it.
- Author and institution entities: `bibliographic-work@1` is works only;
  `bibliographic-author@1` is a later kind with its own `ids` (ORCID) and
  the disambiguation problem the literature calls unsolved.

## What pins this

| Rule | Test |
|---|---|
| Every adapter answers only what its explain record declares; an undeclared index is a diagnostic, never a fallback | conformance test over each adapter's fixtures: every `bib.*` index, declared or diagnosed |
| A canonical query maps to a native one for every adapter that declares its indexes, and the native query round-trips the same results | per-adapter recorded-response fixtures, Tier A |
| A preprint and its published version merge into one work with two locations when they share an id; never on title alone | merge tests over OpenAlex/Crossref/arXiv fixtures of known pairs |
| The scorer reproduces today's acceptance decisions on imbib's resolution fixtures, and every answer carries its evidence | golden suite ported from `AutomationService` tests |
| A search with one slow source returns the others' rows and a `timed-out` diagnostic within the timeout | federation test with a stub adapter |
| A 429 from a source produces `rate-limited` for that source and no retry storm; limits read from headers update the bucket | etiquette tests with recorded 429 and header responses |
| An imported row carries `source_id`, `produced_by` and the source's own BibTeX | import test; `schema-refs.json` names the writer |
| The generated MCP tool for `search` bounds results at 10 by default and every row carries `ids` | descriptor and example tests |

## Rollout

1. **The kind and the explain schema** (`bibliographic-work@1` as a
   `KindSchema`; the explain record's JSON Schema; the `bib.*` context set).
   One week.
2. **`source-service` with one adapter**, Crossref (keyless, polite pool,
   cursor paging, SBMV precedent): explain, search, fetch, match, export.
   Two weeks; it is the proof of S-D1–S-D3 and S-D8.
3. **Port the four live Rust adapters** (arXiv, ADS, OpenAlex, then Crossref
   replaces its own) and **write the three Swift-only ones** (PubMed, WoS,
   SciX as the ADS adapter with a different endpoint) as packages, each
   with recorded fixtures. Three weeks.
4. **The scorer and cascade in Rust** (S-D6), golden-tested against the
   Swift fixtures; `resolve_citation` stops forwarding to Swift. One week.
5. **Federation and etiquette** (S-D7, S-D8). One week.
6. **imbib adopts**: `import-work`, provenance, the smart-search row holds
   a query, the Swift `SourcePlugin` tree deleted. Two weeks, with the
   live-call discipline the suite already uses (every ported tool is called
   against the running app; empty results look like success).
7. **Semantic Scholar, DBLP, INSPIRE** as new packages: the proof that the
   next source costs only its own adapter.

## Open

- **`native` passthrough and review.** A native query can do things the
  canonical grammar cannot (ADS second-order operators), and an agent given
  passthrough can spend a daily quota in one call. Whether `native` is
  `external` with a budget check, or a declared adverb, is TR-D11's risk
  question and is decided there.
- **Which fields are `custom`.** CSL-JSON refuses unknown top-level keys;
  everything of ours is under `custom`. Whether `ids`, `locations` and
  `provenance` live under `custom` on the wire (strict CSL) or beside the
  CSL body in the kind (readable) is a wire-format choice; the kind is the
  same either way.
- **Scores across sources.** `match` over several sources returns several
  scored candidates; the scorer is ours, so its scores *are* comparable,
  unlike the sources' relevance. Whether the best-of-all is auto-accepted by
  the same rules as best-of-one needs the golden suite's answer.
- **The common part with the run convention.** Both conventions have: a
  declaration the thing emits (schema / explain), a canonical kind, a fixed
  generic verb set, adapters as packages, diagnostics over silence,
  provenance kept at the boundary, etiquette or budgets enforced once. That
  is the "domain convention" pattern, and it is lifted into a short document
  of its own once both are built, not before.
