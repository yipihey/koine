# Domain conventions: the pattern the run and source conventions share

**Status:** lifted 2026-10-20 from two built conventions —
plan-run-convention.md (R1–R7 landed: schemas, `run-check`, RunConvention.jl,
titan and Vespa adopting, the importer, `run-service`, the GADGET-4 adapter)
and plan-source-convention.md (S1–S7 landed: the work kind, ten adapters,
the scorer, federation and etiquette, imbib adopting). The user's rule was
that this document could exist only once both were built, so that the
common part is what was *found* to be common, not what was guessed. What
follows is that, plus the eight things the builds taught that neither
design said.

## The pattern

A domain convention is a file-based, koine-independent specification of how
a thing in some domain declares itself, written so that koine can use any
conforming thing with no per-case wrapper. Every one so far has the same
four layers, and a third convention should expect to have them too:

| Layer | Run convention | Source convention | What it owns |
|---|---|---|---|
| **The declaration** | a parameter schema the code emits (`--schema`), JSON Schema + `x-run` | an explain record the adapter emits, JSON Schema | *What can I be asked?* Declared once, where it is read (R-D2.0), never a second list kept by hand |
| **The record** | the run directory (`run.json`, `params.json`, `events.jsonl`, `outputs.json`, `control/`) | the canonical row, kind `bibliographic-work@1.0.0` | *What did I do / what do I hold?* One kind, versioned, with its unknown kept (TR-D13) |
| **The generic verbs** | `run-service_{codes,form,start,status,events,set,stop,checkpoint,resume,sweep,ingest,…}` | `bib-source-service_{explain,search,fetch,match-citation,related,export}` | One `#[impress_service]` over the declaration and the record; the same verbs for every member of the domain |
| **The adapters** | RunConvention.jl for in-house codes; `gadget4-adapt` for a public code | one crate per database, registered into one process-wide registry | Where the domain's irregularity lives. Each costs only itself (S7 proved it: three adapters, no change outside them but one id table and the host's `register()` list) |

Two further pieces recur and belong to the kernel, not the convention:

- **A checker** (`run-check`, `Work::validate` + `ExplainRecord::validate`) that
  a producer can run without koine. It is what let two implementations of the
  run convention — Rust R1 and Julia R2, built in parallel from the same text —
  find the two places the text was ambiguous (R-D6.5a) on the day they met.
- **An importer** (`run-import`; for sources, the `imported` provenance) for
  the things that existed before the convention: they are written *beside*
  the old files, never moved, and a record whose full input cannot be
  recovered says so (`key_withheld`) rather than guessing a key.

## What the builds taught

These were found by adopters and adapters, not by the designs, and each was
settled by amending the design document in the same commit as the fix.

1. **Non-finite floats are the strings `"Infinity"`, `"-Infinity"`, `"NaN"`,
   in the hashed canonical form too** (R-D4.3). JSON has no such number;
   the Julia library refused `Inf`, titan and Vespa both pass it ("no
   limit"), and Vespa's driver had grown a copy of the key path to cope. A
   sentinel is not an answer when production configs already say `Inf`.
2. **A value at its declared default is not "set"** (R-D2.6 `requires`).
   The check skipped source=default, not value=default, so a resume that
   read the parent's file tripped on a constraint the parent never
   exercised.
3. **A checkpoint belongs to the run that last wrote it**, not the nearest
   `run.json` above it (R-D4.2). A child that resumes in place rewrites its
   parent's file; `resume()` now matches the file's hash against the
   checkpoint events of the runs below, and Vespa's replica of `resume`
   went away.
4. **A launched process that has not yet said `running` is still live**
   (R-D4.4). GADGET-4 writes its banner first; a resume in that window
   raced it and a process that died before the banner was never orphaned.
5. **The status set names every outcome a caller must tell apart**
   (S-D7): `not_found` and `error` were being smuggled through `done` with a
   detail string and `refused`, and the service told a miss from an answer
   by string-matching. A miss is not an answer; a 5xx is not the source
   saying no.
6. **A source's own record id is an identifier kind** (S-D4). INSPIRE's
   recid had nowhere to go on a row, so the row could not say which record
   it was and federation could not merge on it. The rule for the closed
   list: it holds every kind some source *assigns*, never one chosen id.
7. **An unrecorded outcome is not reusable** (R-D4.5). The exit bands have
   no neutral code, so an imported run whose outcome nobody recorded was
   `code 0`; `invalidates_reuse` is what stops a tool from taking it as the
   answer for its key.
8. **Name the direction of a relation** (S-D2). `cites = X` is works citing
   X, `cited_by = X` is X's reference list; ADS, OpenAlex and Crossref each
   spell it differently and the plan had not said.

And one about names rather than semantics: **a generic service's trait name
is a tool prefix, and prefixes are global across every linked repository.**
koine's `SourceService` collided with impress-apps' content-provenance
`SourceService` and the verb census counted them as one service; the trait is
`BibSourceService` now, the crate keeps its name. Check the prefix against
every consumer before the first adapter exists, not after seven.

## What stays per domain

The pattern does not predict these, and a third convention should decide
them early, in its own words:

- **The mutability vocabulary.** Runs have `never | restart | always`
  because a code has three ways of accepting a change. A database has none
  of that; it has `operations` and `indexes` with `relations`. The
  declaration's `x-<domain>` block is where this lives.
- **What "the same" means.** A run's identity is a hash of its inputs
  (R-D4.3); a work's identity is any shared identifier (S-D7). The first
  gives reuse, the second gives merging. Neither transfers.
- **Etiquette.** A database has a polite pool, a budget and a 429; a code
  has a wall-clock limit and a stop file. Both are "what the thing asks of
  its caller", enforced once in the generic service, but the shapes differ.
- **The specialisation.** The source convention was written to "remain
  general yet specialise perfectly for imbib"; the run convention for titan
  and Vespa first, GADGET-4 second. The general part is the four layers; the
  specialised part is the kind and the adapters, and the second domain
  showed that nothing of the first's kind or adapters was reusable.

## How a third convention starts

1. Name the domain's *declaration* (what an instance can be asked) and its
   *record* (what it did or holds). If either needs koine to make sense, the
   convention is not independent yet.
2. Write the two JSON Schemas and a checker a producer can run alone.
   Build the checker twice if you can afford it (two languages, two
   people); the disagreements are the design's holes.
3. Write the generic service as one `#[impress_service]` — the verbs that
   hold for every member — and the first adapter in the same package, so
   the service is shaped by a real member and not by the plan.
4. Adopt it in an in-house instance before a public one: the in-house one
   finds the library's gaps (items 1–3 above all came from titan and Vespa),
   the public one finds the contract's (item 4, and the three GADGET-4
   notes in R-D13.1).
5. Prove the marginal cost with a member nobody asked for (S7's rule:
   nothing outside the new adapter may change). Where it does, the thing
   that changed is the convention, and the amendment goes in the design
   document in the same commit.
