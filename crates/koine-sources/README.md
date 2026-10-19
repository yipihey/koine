# koine-sources

The source convention's vocabulary (`docs/plan-source-convention.md`, work
package S1): how an online database declares itself to koine. Types only; no
HTTP and no adapters. An adapter crate depends on this one.

| Module | What |
|---|---|
| `explain` | `ExplainRecord`: indexes, relations, sortable, identifiers, fills, page, etiquette, secrets, native (S-D1) |
| `query` | the `bib.*` indexes and relations, the JSON `Query`, `covered_by` (S-D2) |
| `kind` | `bibliographic-work@1.0.0`: the schema, the typed `Work`, `to_csl_json` (S-D4) |
| `diagnostics` | `Diagnostic { source, status, detail }` (S-D7) |

The kind is spelled `bibliographic-work@1.0.0` in `schema-refs.json`, like
every versioned kind; an explain record may say `bibliographic-work@1`.
Non-CSL fields (`ids`, `locations`, `provenance`, `metrics`) sit beside the CSL
body; `to_csl_json` emits strict CSL with them under `custom`.

## 1. An explain record

```json
{"source": "crossref", "name": "Crossref", "version": "rest-v1",
 "kind": "bibliographic-work@1", "operations": ["search", "fetch"],
 "indexes": {"title": {"relations": ["any", "adj"]},
             "doi":   {"relations": ["="], "exact": true}},
 "identifiers": ["doi"], "page": {"model": "cursor", "max": 1000}}
```

```rust
let record = koine_sources::ExplainRecord::from_json(text)?; // parses and validates
```

## 2. A query and its diagnostics

```rust
let q: Query = serde_json::from_str(
    r#"{"and":[{"index":"first_author","relation":"any","term":"Abel"},
              {"index":"title","relation":"any","term":"reionization"}]}"#)?;
let diagnostics = q.covered_by(&record);
// [{"source":"crossref","status":"unsupported","detail":"bib.first_author"}]
```

An index the record does not declare is a visible diagnostic, never a
silent fallback to another index.

## 3. A work, as stored and as strict CSL

```rust
koine_sources::ensure_registered();            // the kind, once per process
let work: Work = serde_json::from_value(row)?; // a bad location version is refused
let csl = work.to_csl_json();                  // CSL keys; ids/locations under `custom`
```
