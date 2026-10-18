# Koine and its relatives: prior art, comparison, lessons

A survey of systems close in spirit to koine, done 2026-10-17 in six
families: malleable software and semantic desktops; verb, command and
capability systems; UI as data and layouts; provenance and lab notebooks;
agent-native frameworks and agent security; item stores and local-first
sync. About 150 systems were looked at; the ones that teach something are
below. Each lesson is marked against koine as it is today:

- **have** — koine already does it
- **part** — koine does some of it; the gap is named
- **gap** — koine does not do it

## 1. What koine bets on, in one paragraph

People and agents speak one typed grammar over one store, through any
frontend. A verb is declared once (schema, effects, safety, secrets,
`returns_kind`) and the same declaration becomes HTTP, MCP, CLI, a generated
form, a skill and a task. Adverbs and consent are applied by one pipeline,
not by each verb. Every record is an item behind a port with three
conformance-tested backends. Every call is logged in that store with its
call tree, reads and writes. UI is data performed by three native frontends.

No surveyed system makes all of these bets at once. Many make two or three.

## 2. The nearest relatives

| System | Shares with koine | Differs | The one thing to take |
|---|---|---|---|
| **Elixir Ash + AshAi** | One action declaration fans out to code, JSON:API, GraphQL, policies, LLM tools and an MCP server | Resource/CRUD-centric; no adverbs, no call log | AshAi exposes an *explicit allow-list* of actions to models, not all of them |
| **Apple App Intents** | Verbs (`AppIntent`), nouns (`AppEntity` + `EntityQuery`), pointing (on-screen entity annotations), `UndoableIntent`, `requestConfirmation`/`requestChoice` | Platform-owned; no store, no provenance | Generate App Intents and Entities from koine's inventory: Siri, Shortcuts and Spotlight for free |
| **PowerShell** | `-WhatIf`/`-Confirm` added by the framework (`SupportsShouldProcess`), `ConfirmImpact` vs `$ConfirmPreference` | Shell-only | `-WhatIf` silently runs real changes across module boundaries (an open RFC for years): dry run must be enforced below the verb |
| **MCP / OpenAI Apps SDK / A2A / ACP** | Tool descriptors, annotations, elicitation, tasks, permission options | Annotations are self-declared and untrusted; no call log | Map koine's verified effects outward losslessly; use their task states for review-pending and missing secrets |
| **Haystack (MIT)** | Data, commands, views all described as data; views per type; several contexts at once | RDF triple store; slow; never daily-usable | Keep "operations are data", not the generic triple machinery |
| **Glamorous Toolkit** | Many cheap views per object kind; actions on views | Image-based, Smalltalk | Make views per kind a declared, ~10-line extension point like verbs |
| **Plan 9 plumber / Acme** | Pointing → action | Text and regexes | Rules, stored as items, that route a selection of kind K to verb V in pane Q |
| **FDC3 (trading-desktop linking)** | Typed contexts on channels between panels; intents on what's selected | Finance-only | Last value kept per channel, no echo, visible channel colour, intents resolved by a resolver |
| **A2UI / json-render / Adaptive Cards** | UI as data, native renderers, agent-written UI | No store; no person's copy | Per-frontend catalogs with versions; per-node `requires` + `fallback`; prompt generated from the same catalog |
| **AiiDA** | A mandatory provenance graph of calculations and workflows | Python, HPC | Separate *created* from *returned*; data lineage vs logical lineage are two views |
| **Galaxy / VisTrails** | History → workflow extraction; history as a version tree | Pipelines, not verbs over a store | `save-macro` is Galaxy's "extract workflow" — with automatic parameter inference |
| **Temporal / Restate** | Durable steps, retries, replay | Workflow code, not a store | Journal nondeterministic results so replay is meaningful |
| **Simon Willison's `llm`** | Every prompt, response and tool call logged to SQLite | One user, no policy | Make the notebook plain-SQL/Datasette-queryable; redact secrets before logging |
| **AT Protocol** | Kinds ≈ Lexicon NSIDs, items ≈ records, schemas are records, blobs by reference | Public, signed, federated | Lexicon's evolution rules verbatim for kinds |
| **Zotero** | Items of item types, fields, relations, attachments beside SQLite, versioned sync | One domain | Field-level three-way merge for *every* kind; reuse its `schema.json` and CSL-JSON for imbib |
| **Datomic** | Immutable log; transactions are entities | Server database | Call records as queryable entities; `as_of(call)` views |
| **Ink & Switch (Patchwork, Cambria, Embark, Potluck, essay)** | Shared data under many tools, agents as tool-makers, gentle slope | Documents and CRDTs, not verbs | Drafts/branches for `tentatively`; lenses for kind evolution; a slope above Rust |

## 3. Where koine is already ahead

These are real, and worth saying plainly when positioning koine:

1. **Declared effects are verified.** MCP and OpenAI annotations are hints
   clients must not trust; koine's spy store checks every verb's declared
   reads and writes against what it does. Nothing else surveyed does this.
2. **One envelope for adverbs and consent.** PowerShell, Kubernetes and
   AIP-163 converge on this design; koine has it across every transport.
3. **Typed verbs instead of screen-driving.** Computer use, browser-use and
   UI Automation are what koine makes unnecessary; the Windows agent
   registry, Android AppFunctions and App Intents are moving the same way.
4. **Provenance by construction.** Every call is in the store with its tree
   and reads/writes. AiiDA is the only other system where provenance is
   mandatory; most notebooks and agent frameworks bolt it on.
5. **A human-legible backend that is a conformance-tested peer.** Logseq
   moved the other way and lost users; Obsidian's files are its moat.
6. **People and agents as distinct callers.** Windows' agent accounts,
   Android's `EXECUTE_APP_FUNCTIONS` and 12-factor agents ask for exactly
   this; koine has it.

## 4. Lessons by koine part

### 4.1 Verbs and adverbs

- **`tentatively` must be enforced below the verb** (part). Today a verb
  that declares `supports(tentatively)` runs and must write nothing; the
  store does not refuse. Do as Kubernetes does: the store port refuses or
  rolls back writes under a dry run, the flag propagates through every
  sub-call, and a dry run whose call tree reaches a verb that cannot honour
  it (an `External` verb) is refused up front. For store-only verbs, run and
  roll back (PostgREST `tx=rollback`) and return the real diff. Same
  validation, same policy, same result shape (Google AIP-163).
- **`once` as the IETF Idempotency-Key draft and Stripe do it** (part).
  Today it keys on (caller, key) and stores successes only. Add an argument
  fingerprint (reuse with different arguments → mismatch, 422), an in-flight
  conflict (409), store failures too, and publish a retention window. Task
  retries derive the key from the task id (Temporal).
- **`reversibly` records the inverse when it runs** (part — the adverb
  exists). Apple's `UndoableIntent` registers the inverse at execution, not
  later.
- **`within` is a deadline that propagates to sub-calls;** `in_background`
  heartbeats progress into the task record (Temporal).
- **Argument-aware review** (gap). Besides the static safety class, an
  optional `needs_review(args, store)` hook (Raycast): deleting a draft
  needs no review, deleting a cited paper does.
- **Per-target confirmation with alternatives** (gap). "Delete these 12
  items" lists them and may offer "archive instead" (App Intents
  `requestChoice`, Django admin's confirm page).
- **Version every verb and stamp it on every call** (part — `since` and
  deprecation exist). Hash the descriptor; put the hash in the call record,
  the skill and MCP. A change of effects or safety invalidates standing
  grants (the MCP "rug pull").
- **Verbs filtered by what is pointed at** (gap). Emacs had to add command
  modes after M-x exploded; VS Code has `when` clauses; Tana attaches
  commands to supertags. Filter the MCP list, the skill and the palette by
  the selection's kind, and say why a verb is unavailable (LSP
  `disabled.reason`).
- **Results name the next applicable verbs** (gap; HATEOAS, GT actions).

### 4.2 Consent, capabilities and agent security

The most important family of lessons, because koine's declared effects let
it enforce what other frameworks can only advise.

- **Enforce the lethal trifecta / Rule of Two per session** (gap). Extend
  effects with *reads private data*, *ingests untrusted content* (fetch,
  PDF import, email) and *communicates externally* (`External` already
  covers part of this). Once a session holds all three, egress and writes
  escalate to review automatically (Willison; Meta's "Agents Rule of Two").
- **Provenance-tagged values** (gap). A value a verb returns carries its
  source item or call and whether it came from untrusted input; policies can
  say "a recipient may not come from an imported PDF" (CaMeL, lite). For the
  built-in model, a plan-then-execute mode: the person approves one plan,
  not N calls.
- **Server-side policy is authoritative** (have — keep it so). No client
  hook, annotation or agent self-report may downgrade a review. (Claude Code
  had a hook-`allow`-overrides-deny bug.)
- **Scoped, batched, durable grants against consent fatigue** (gap).
  Impact level × caller threshold (PowerShell `ConfirmImpact`); ACP's
  `allow_once`/`allow_always`/`reject_once`/`reject_always`, with "always"
  written as a visible, revocable rule item; grants by class × scope ×
  duration ("reversible writes in this project for this session"); a batched
  review queue; an optional LLM risk rater that may only *escalate*
  (OpenHands).
- **Attenuated capabilities instead of ambient authority** (gap). Per
  frontend and per agent capability sets that fail closed (Tauri, but
  default-deny); an explicit agent-exposed allow-list (AshAi, Home
  Assistant); time-boxed grants over named items (Cap'n Proto, E); a
  loopback token scoped to a capability set.
- **Pending states as resumable tasks** (part). Review-pending is MCP
  `input_required` / A2A `INPUT_REQUIRED`; a missing secret is
  `AUTH_REQUIRED`; a policy refusal is `REJECTED`, distinct from `FAILED`.
  Resume with an approval token, never re-prompt (Pydantic AI deferred
  tools). Secrets go through URL-mode elicitation, never the agent.
- **Pin what agents read as instructions** (part — packages are pinned).
  Hash the generated SKILL.md, verb descriptions and package descriptions;
  show a description diff on package update; unreviewed packages only in a
  developer mode (Windows agent registry). Item contents returned to agents
  are untrusted data and never go into skills or descriptions.

### 4.3 The store

- **Kind evolution by AT Protocol's Lexicon rules** (gap). New fields
  optional; no removing required fields, changing types or closing unions
  further; a breaking change is a new kind name. Reverse-DNS kind names for
  third-party packages. Stamp the kind's schema version on every operation
  so Cambria-style read-time lenses stay possible. This matters doubly
  because devices sync at different app versions and CloudKit only extends
  schemas.
- **Unknown attributes and unknown kinds survive every round-trip** (gap in
  the conformance suite), including "data arrives before its schema item".
- **A merge strategy per attribute** (gap): lww, set-union, counter,
  text-CRDT (Automerge or Loro, both Rust, for imprint's rich text only),
  ask. Generalise the shipped/yours three-way merge to sync (Zotero): keep
  the last-synced base, merge disjoint field edits, pick a deterministic
  winner for same-field conflicts and *keep the loser* as a queryable
  conflict (CouchDB).
- **Transactions are items; cursors are per consumer** (part — ops carry the
  call id). A call/batch record with actor, agent, model and conversation;
  `as_of(call)` views; per-consumer opaque cursors (site id + db version,
  cr-sqlite); compaction only below the slowest cursor; tombstones in
  `arrivals_after` (Core Data history, Zotero `/deleted`).
- **The dir format as the durable interchange format** (part). Deterministic
  serialisation, no derived data in item files, per-device append-only log
  files; "rebuild SQLite from dir" as a tested recovery path (Fossil,
  Calibre). Decide explicitly whether external edits to the dir backend are
  first-class (Logseq: that is the hard part).
- **Performance budgets in the conformance suite** (gap). NEPOMUK and
  Haystack died of latency and background CPU: cold open, query latency,
  `references_to` and indexing cost on an iPhone; low-priority, visible
  background work.
- **Union of stores** (gap). Present the shipped package store under the
  person's store as one queryable union, like Newton's union soups or a
  Plan 9 union mount: shipped-vs-yours expressed structurally.
- **System integration** (gap). Donate `search_text` to Core Spotlight by
  kind; FTS5 and region-scoped live queries (GRDB) so panes re-run only when
  their kinds change.
- **Reuse domain standards** (gap). Zotero item types and CSL-JSON for
  imbib's kinds, with an open "extra" bag (Zotero); attribute names with a
  vault-wide type registry (Obsidian properties).

### 4.4 The lab notebook and provenance

- **Journal nondeterministic results; two replay modes** (part —
  `replay_full` exists). Re-execute and compare output hashes (Sumatra), or
  replay from the journal, injecting recorded LLM, network and clock
  results (Temporal, Restate). Mark verbs deterministic or not. Replays run
  under `tentatively` or with `once` keys so they never re-fire effects
  (LangGraph's documented pitfall).
- **Created vs returned** (gap). A composite verb that returns an item it
  did not write must not claim it; then `why` points at the call that made
  the bytes. Offer both data lineage (`why`) and logical lineage (`trace`)
  (AiiDA).
- **`save-macro` as Galaxy's extract-workflow** (part). Pre-select the
  calls, drop failed and undone ones, propose parameters (items read but not
  written inside the selection), and let each argument bind absolutely or
  relatively ("this item" vs "the selection" vs "items of kind K") — the
  classic macro-recorder failure is absolute references.
- **`propose-workflows` with process-mining hygiene** (part). An explicit
  case key (session, run or root item); activities abstracted to verb +
  argument kinds; minimum support and a noise threshold; export OCEL so
  PM4Py and ProM work on it.
- **Staleness** (gap). "This plot came from verb v3 over data since
  superseded" (Dagster) — falls out of provenance and is very valuable for
  implore.
- **History as a tree of explorations with tags** (gap; VisTrails), and
  "fork from here": rerun a logged call with edited arguments, tentatively.
- **Publishable export** (gap). A Provenance Run Crate (RO-Crate: call →
  `CreateAction`, verb → `instrument`, reads → `object`, writes → `result`,
  agent-for-person → `prov:actedOnBehalfOf`) with stable store, run and call
  ids; PROV-O and OpenLineage as secondary; a Zenodo verb. ELN-format import
  into lab notebooks (eLabFTW, RSpace). A redaction review before anything
  leaves the store (ReproZip's editable pack).
- **Volume and privacy from day one** (part). Reads as hashed sets; compact
  trivial read-only calls; current state materialised apart from the log;
  filtered views (Datomic `filter`); agent transcripts opt-in on export.
- **Sign and lock entries** (gap). ELNs: a signed, timestamped entry is
  immutable after sign-off; a verb over the attributed log.

### 4.5 Surfaces, layout and pointing

- **Per-frontend catalogs with versions** (part — plots report what they
  drop). Each frontend declares the node kinds and versions it draws; nodes
  carry `requires` and `fallback`/`drop` (Adaptive Cards); conformance checks
  each frontend drops exactly what it declares. This answers lowest-common-
  denominator UI without hiding it.
- **The agent's surface vocabulary is generated from that catalog**, and an
  agent-written surface is validated against it before it is stored
  (json-render, A2UI).
- **One action model from the start.** Adaptive Cards suffered more from
  fragmented action semantics than from missing widgets.
- **Agent edits as JSON Patch with `test` preconditions** (gap; AG-UI), and a
  version on the surface spec itself: the three-way upgrade becomes
  auditable and mergeable.
- **Security of agent-made surfaces** (part). No executable content; one
  sandboxed, web-only escape hatch with a stated fallback elsewhere (MCP
  Apps); verbs callable from surfaces by opt-in (Apps SDK
  `widgetAccessible`); a destructive verb's risk drawn from its descriptor,
  never from a label the agent wrote.
- **Pointing modelled on FDC3** (part). Typed contexts (the row kind), the
  last value per channel for late joiners, no echo to the sender, an
  always-visible channel indicator, intents on what is pointed at. Selections
  as ids *or as a predicate* (Glue subset states, Vega-Lite field
  projections) so a brush in a plot and a selection in a table share one
  channel. Bridge "this" to `NSUserActivity.appEntityIdentifier` so Siri sees
  it too.
- **Forms: data schema plus an optional UI schema** (gap; JSON Forms), with
  ranked per-frontend renderers so a SwiftUI date picker wins over a text
  field; overrides stored as items.
- **Placement as rules** (gap). Emacs `display-buffer-alist` and zellij swap
  layouts: "where does this result open" and re-tiling as data in the
  gesture grammar. Layouts stored as items (Blender keeps them in the
  document).
- **Views per kind as an extension point** (gap; Glamorous Toolkit): cheap,
  many, rendered by all three frontends. And a "which views can show this
  row kind" switch per pane (Airbnb Ghost's section vs component type).

### 4.6 Agents, memory and the desk

- **Wishes, generalised** (part — the desk). Dynamicland's claims/wishes/when
  and 12-factor agents' "contact humans with tool calls": a review-pending
  call or an unassigned request is a wish item any capable party may claim,
  and *agents* can file desk items for a person when blocked.
- **The desk can launch agents** (gap). Beyond being polled
  (`agent-service_next`), act as an Agent Client Protocol client: start
  Claude Code, Codex or Gemini CLI, stream progress into the desk, route
  their permission requests into koine's review.
- **Memory with provenance, validity time and invalidation** (part). Agent-
  derived facts are marked as such with their source call, invalidated
  rather than overwritten (Graphiti's bi-temporal model); person-visible,
  editable memory blocks (Letta); consolidation as a sleep-time task.
- **Claims cite their support** (gap). Agent-produced statements link to the
  calls or items behind them (FutureHouse Kosmos).
- **Evals for routing and task categories** (gap). Log (category, tier,
  outcome, the person's correction) and measure the on-device router rather
  than guess (DSPy, Mastra).
- **Interop by generation** (part). From the one inventory: Agent Skills-
  conformant SKILL.md (validate the name and description limits; the full
  inventory in `references/` for progressive disclosure), AGENTS.md for
  Codex, MCP `outputSchema` and `structuredContent` everywhere, OpenAPI for
  `/api/verb/*`, App Intents. Progressive disclosure (`search_verbs`,
  `describe_verb`) once inventories get large.
- **Don't own eight provider clients if you don't have to.** Rig's
  provider traits (Rust) could carry the AI port's HTTP side; koine's value
  is the tool loop, policy and provenance.

### 4.7 The slope above Rust

The malleable-software essay, HyperCard's user levels, Potluck, Embark and
Coda Packs all say the same thing: between "use a verb" and "write a Rust
crate" there must be rungs. Koine's candidates, all items so agents can make
them and people can review them: saved queries; adjectives defined as data
(Lotus Agenda: a category is both a predicate and a tag); verb compositions
and recipes with success criteria (Arazzo); plumbing rules; editable view
specs; and agent-made tools that arrive as reviewed packages. Expose levels
by safety class, so a newcomer sees a small, safe vocabulary.

## 5. Warnings from the systems that failed

| System | What happened | For koine |
|---|---|---|
| WinFS | Cancelled; desktop search gave most of the benefit far more cheaply | Ship cross-app search across kinds first; never gate value on the full grammar |
| Chandler | Years of architecture before a usable app (*Dreaming in Code*) | Let imbib, imprint and implore pull features into koine; nothing for symmetry alone |
| Haystack, NEPOMUK | Generic semantic stores, too slow; NEPOMUK replaced by SQLite-based Baloo | Performance budgets in conformance; typed kinds, not triples |
| Fuchsia Modular | An OS-level intent/entity composition model with no apps driving it, removed | Don't make the grammar an OS layer before the apps demand it |
| OpenDoc | Embedded live components across vendors: memory, complexity, no business model | Panes are query + spec, never foreign processes |
| Darklang classic | A bespoke editor and hosting became a liability; rewritten | Stay on ordinary editors, Rust and MCP; keep trace-driven development |
| ElectricSQL (v1) | Active-active sync too complex to stabilise; retreated to read-path sync | Local canonical store, op log, sync as a separate adapter |
| Logseq DB | Moving canonical data from files to a DB split the community | The dir backend stays a first-class peer |
| HyperCard | No network or sharing story; owner stopped investing | A shared substrate needs an owner and a sharing story |
| Realm Device Sync | Vendor sync service deprecated | Owning the sync adapter is justified |
| Adaptive Cards | Hosts drifted into incompatible action models | One action model, one version scheme, from the start |
| PowerShell `-WhatIf` | Dry run silently not honoured across module boundaries | Enforce adverbs in the store and pipeline, never by convention |

## 6. The recommendations, ranked

Ordered by value to koine over cost. Each is a candidate work package.

1. **Session taint and the Rule of Two.** Add *private*, *untrusted-ingest*
   and *external* to effects; track them per session; escalate
   automatically. Koine is the rare system that can enforce this
   structurally. (§4.2)
2. **Store-enforced `tentatively`, idempotency-key-grade `once`.** Dry run
   refused or rolled back at the port and propagated through call trees;
   `once` with argument fingerprints, in-flight conflicts and stored
   failures. (§4.1)
3. **Kind evolution rules and unknown-field round-trip.** Lexicon rules in
   the registry, schema version on every op, conformance tests for unknown
   attributes and kinds. Cheap now, very expensive after devices diverge.
   (§4.3)
4. **Consent that people don't learn to click through.** Impact × caller
   threshold, argument-aware review, per-target lists with alternatives,
   durable revocable "always" rules, a batched queue, resumable pending
   states (`input_required`, `auth_required`, `rejected`). (§4.1, §4.2)
5. **App Intents from the inventory**, with pointing bridged to
   `NSUserActivity`. The highest-leverage new frontend for the Apple apps.
   (§4.6)
6. **Replay that means something.** Journal nondeterministic results, mark
   determinism, verb hash on every call, created-vs-returned, staleness.
   (§4.4)
7. **Pointing as FDC3 plus plumbing rules.** Typed contexts, last value,
   predicates as selections, visible channels, and stored rules from
   selection to verb and pane. (§4.5, §4.7)
8. **Surface catalogs with `requires`/`fallback`** and agent edits as
   validated patches. (§4.5)
9. **Field-level merge and per-attribute strategies for sync**, conflicts
   kept rather than lost. (§4.3)
10. **Publishable provenance**: RO-Crate run crates with a redaction review,
    and a Zenodo verb. Scientists' natural exit. (§4.4)
11. **`save-macro` and `propose-workflows` done the Galaxy and
    process-mining way.** (§4.4)
12. **Rungs above Rust**: saved queries, data-defined adjectives, recipes,
    views per kind. (§4.7)

## Sources

The most load-bearing ones; each family's survey had more.

- Malleable software: https://www.inkandswitch.com/essay/malleable-software/ ·
  https://www.inkandswitch.com/cambria/ · https://www.inkandswitch.com/embark/ ·
  https://www.inkandswitch.com/patchwork/notebook/ ·
  https://scg.unibe.ch/archive/papers/Chis15a-MoldableInspector.pdf ·
  https://haystack.csail.mit.edu/papers/www03-ui.pdf · https://9p.io/sys/doc/plumb.html ·
  https://folk.computer/notes/internals/db · https://arxiv.org/abs/2512.09762
- Verbs and capabilities: https://modelcontextprotocol.io/specification/2025-06-18/server/tools ·
  https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks ·
  https://developers.openai.com/apps-sdk/reference · https://a2a-protocol.org/latest/specification/ ·
  https://agentclientprotocol.com/protocol/tool-calls · https://ai.pydantic.dev/deferred-tools ·
  https://learn.microsoft.com/en-us/powershell/scripting/learn/deep-dives/everything-about-shouldprocess ·
  https://github.com/PowerShell/PowerShell-RFC/pull/221 · https://google.aip.dev/163 ·
  https://google.aip.dev/155 · https://kubernetes.io/docs/reference/using-api/api-concepts/#dry-run ·
  https://datatracker.ietf.org/doc/html/draft-ietf-httpapi-idempotency-key-header ·
  https://docs.stripe.com/api/idempotent_requests · https://developer.apple.com/videos/play/wwdc2025/275 ·
  https://hexdocs.pm/ash_ai · https://v2.tauri.app/security/capabilities/ · https://capnproto.org/rpc.html ·
  https://developers.raycast.com/ai/learn-core-concepts-of-ai-extensions ·
  https://docs.postgrest.org/en/stable/references/transactions.html
- UI and layout: https://a2ui.org/specification/v1.0-a2ui-extension-specification/ ·
  https://modelcontextprotocol.io/seps/1865-mcp-apps-interactive-user-interfaces-for-mcp ·
  https://github.com/vercel-labs/json-render · https://docs.ag-ui.com/concepts/state ·
  https://learn.microsoft.com/en-us/adaptive-cards/authoring-cards/universal-action-model ·
  https://fdc3.finos.org/docs/api/spec · https://glueviz.readthedocs.io/en/stable/developer_guide/selection.html ·
  https://zellij.dev/documentation/swap-layouts · https://docs.blender.org/manual/en/4.2/editors/index.html ·
  https://textual.textualize.io/blog/2024/09/08/towards-textual-web-applications/
- Provenance: https://www.w3.org/TR/prov-o/ · https://www.researchobject.org/workflow-run-crate/profiles/ ·
  https://aiida.readthedocs.io/projects/aiida-core/en/latest/topics/provenance/concepts.html ·
  https://training.galaxyproject.org/training-material/topics/galaxy-interface/tutorials/history-to-workflow/tutorial.html ·
  https://docs.temporal.io/workflow-definition · https://docs.restate.dev/concepts/durable_execution ·
  https://openlineage.io/docs/spec/object-model · https://docs.datomic.com/reference/filters.html ·
  https://handbook.datalad.org/en/latest/basics/101-108-run.html · https://sumatra.readthedocs.io/
- Agents: https://agentskills.io/home · https://docs.langchain.com/oss/python/langgraph/use-time-travel ·
  https://arxiv.org/abs/2511.03690 · https://github.com/humanlayer/12-factor-agents ·
  https://every.to/guides/agent-native · https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/ ·
  https://ai.meta.com/blog/practical-ai-agent-security/ · https://arxiv.org/abs/2503.18813 ·
  https://arxiv.org/abs/2506.08837 · https://invariantlabs.ai/blog/mcp-security-notification-tool-poisoning-attacks ·
  https://github.com/simonw/llm/blob/main/docs/tools.md · https://arxiv.org/abs/2501.13956 ·
  https://blogs.windows.com/windowsdeveloper/2025/11/18/ignite-2025-furthering-windows-as-the-premier-platform-for-developers-governed-by-security
- Stores: https://atproto.com/specs/lexicon · https://atproto.com/specs/blob ·
  https://www.zotero.org/support/dev/web_api/v3/syncing · https://github.com/zotero/zotero-schema ·
  https://docs.couchdb.org/en/stable/replication/conflicts.html · https://github.com/vlcn-io/cr-sqlite ·
  https://fossil-scm.org/home/doc/trunk/www/fileformat.wiki · https://stephango.com/file-over-app ·
  https://discuss.logseq.com/t/why-the-database-version-and-how-its-going/26744 ·
  https://electric-sql.com/blog/2024/07/17/electric-next
