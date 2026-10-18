# The koine grammar: parts of speech

Koine is the speech people and agents share when they work on the same
store through the same frontends. This document names its parts of speech
and the rules each one follows. It is a contract: every rule below names
the test that keeps it true, and a change to a rule is a change to this
file first.

| Part of speech | What it is | Where it lives |
|---|---|---|
| **Noun** | A record of a kind, or a query over records | `impress-item`, `impress-pane-query` |
| **Pointing** | The selection a pane publishes on a channel: "this", "these" | `impress-layout` |
| **Verb** | A capability with a descriptor: name, arguments, effects, safety | `impress-service-core` |
| **Adjective** | A named predicate over records, usable in any query | `impress-pane-query::adjective` |
| **Adverb** | A modifier on one call: how the verb runs, not what it does | `impress-service-core::adverb` |
| **Mood** | Whether a call asks, does, or wonders: read-only, mutating, tentative | the safety class and the `tentatively` adverb |
| **Consent** | The review step: a call a person must confirm before it runs | `impress-service-core::pipeline::policy` |

Nouns, pointing, verbs and consent are documented in their crates. This
file defines the two parts added on 2026-10-11: adjectives and adverbs.

A verb may also say which noun its rows are:
`#[impress_method(returns_kind = "publication")]` puts the pane-query kind
id on the descriptor (`VerbDescriptor::returns_kind`), shows it in the
`tools/list` entry as `_meta.returns_kind` and labels the generated result
table `Result (publication)`. A surface's table and a layout's pane for that
kind then agree on columns, ids and selection, because both name the kind
rather than guess at it. The macro checks the value is a well-formed kind id
(lowercase letters, digits, `_` and `-`, starting with a letter); that it is
a kind of the host's manifest is the host's check, since the manifest does
not exist at expansion time.

`rows(…)` beside it says which fields of those rows matter:
`#[impress_method(returns_kind = "paper", rows(id = "bibcode", title =
"title", columns = ["bibcode", "title", "year"]))]`. `id` is required and is
the field a selection carries; `title` defaults to the id; `columns` defaults
to the row object's properties in the order the output schema lists them.
`rows(…)` without `returns_kind` on the same method is refused at expansion
(rows are of a kind). The descriptor carries `rows: Option<RowSchema>`, and
`VerbDescriptor::row_schema_check()` reads the output schema (the `items` of
an array result, or the one array-of-objects property of an object result,
following a local `$ref`) and refuses a result that is not rows or an id,
title or column that is not a property of the row object, naming it and the
properties that exist. The Tier A report runs that check for every linked verb
that declares `rows(…)`, so a renamed DTO field fails `cargo test`, not a form.
`tools/list` shows `_meta.rows` (`id`, `title`, `columns`, defaults filled in)
beside `_meta.returns_kind`; the generated result table takes its `columns`
from it, sets `Table.id_field` to the id (absent from the wire otherwise, so
every other surface serialises as before) and writes a select to
`state.selected`.

A verb may also declare the secrets it needs, `#[impress_method(secrets =
["scix-api-token"])]` (`VerbDescriptor::secrets`, `_meta.secrets`). They are
held by the host or the environment (`impress_service_core::secrets`), never
passed as arguments, and the pipeline refuses a call while one is absent, as
`missing-secret` (HTTP 412), before the policy layer. See
[capability.md](capability.md) § 6.

A bespoke app is surfaces over verbs: its
layout's panes are `surface` panes, and no view kind is written in Swift or
Dioxus for it.

## Adjectives

An adjective narrows a set of records without changing any of them. The
query algebra already carries several — the `filters` of a `PaneQuery`
(`read`, `starred`, `flag`, `tag`, `status`, `date-range`) — and the
chassis computes others as row fields. What was missing was one class:
a declared name, the kinds it applies to, and how it is evaluated.

### The declaration

```rust
pub struct Adjective {
    /// The word: `unread`, `starred`, `flagged`, `tagged`, `stale`.
    pub name: &'static str,
    /// Record kinds it applies to (pane-query kind ids). Empty: every kind.
    pub kinds: &'static [&'static str],
    /// Arguments the adjective takes, each with a JSON type.
    pub params: &'static [AdjectiveParam],
    pub description: &'static str,
    pub basis: Basis,
}

pub enum Basis {
    /// Lowered into the algebra: a function from the adjective's arguments
    /// to one or more `Filter`s. The store evaluates it.
    Stored(fn(&Value) -> Result<Vec<Filter>, AdjectiveError>),
    /// Computed by a verb that takes `{id}` and answers `{holds: bool}`. The
    /// host evaluates it per row, after the stored filters, within the
    /// query's limit.
    Derived { verb: &'static str },
}
```

### Rules

1. **An adjective is used by name.** A `PaneQuery` carries
   `adjectives: [{ "name": "flagged", "args": { "color": "red" } }, …]`
   beside its `filters`. Both are allowed; `filters` is the algebra,
   `adjectives` is the vocabulary over it.
2. **Expansion is pure and total.** `expand(query, registry)` returns the
   query with every stored adjective lowered into `filters`, plus the list
   of derived adjectives the host must still apply. An unknown adjective,
   a wrong kind, a missing or ill-typed argument is a typed
   `PaneQueryError`, never a dropped predicate (the algebra's own rule:
   empty is not an error, and an error is not empty).
3. **Negation is a word, not a flag.** `unread` and `read` are two
   adjectives, as `starred` and `unstarred` are. There is no `not:` key,
   because a reader of a query should not have to evaluate anything to
   know what it selects.
4. **The built-in registry lowers to filters the algebra already has.**
   No new `Filter` variant is added by this change. Growing the algebra is
   still an ADR amendment.
5. **A derived adjective is a verb.** It is declared on the verb's
   descriptor — `#[impress_method(adjective(name = "stale", kinds =
   ["publication"]))]` — and its verb must be read-only, take `id`, and
   answer `{ "holds": bool }`. The registry a host builds from its
   inventory (`AdjectiveRegistry::with_inventory`) adds those to the
   built-ins. A derived adjective cannot be pushed into the store; the
   host applies it after the stored filters and says so in the result's
   `truncated`/`applied` fields.
6. **One registry.** The built-in adjectives are declared once, in
   `impress-pane-query::adjective::builtin()`. A test pins that every
   built-in name is unique, every one lowers to a filter the compiler can
   take, and the registry's JSON listing round-trips.

What this change does not do: units and conventions on numeric values
(a different kind of adjective, on a value rather than a record), and
provenance adjectives such as `agent-generated` that need a fact the
store does not index yet. Both are listed in the plan as follow-ups.

## Adverbs

An adverb changes how a call runs without changing what the verb does.
Today those modifiers are arguments on individual verbs, or exist for one
service only. The adverb is the one uniform envelope every verb accepts,
checked in one place: the invoker pipeline.

### The wire

Adverbs ride beside the arguments under one reserved key, `adverbs`,
which no verb's own schema may name:

```json
{ "id": "…", "adverbs": { "tentatively": true, "with_review": true } }
```

The pipeline removes the key before the strict-argument check, so a
verb's arguments are exactly what its schema says, and a misspelt adverb
is `invalid-argument` naming it. An entry path that already has a typed
call (the CLI, a surface action, a scenario step) sets `Call.adverbs`
directly; the key is for the transports that carry one JSON object (MCP,
HTTP).

### The vocabulary

| Adverb | Value | Meaning | Who may use it | Needs |
|---|---|---|---|---|
| `tentatively` | `true` | Do not run; answer what would happen | any caller, any verb | nothing |
| `with_review` | `true` | Queue for a person's confirmation, whatever the policy says | any caller, any verb | a review queue, else `review-pending` with no queue (as today) |
| `once` | key string | Idempotent under retry: a repeat with the same key and arguments answers the first result | any caller, any verb | nothing in-process; koine-host installs a durable `IdempotencyStore` (`koine/once@1.0.0` rows) |
| `reversibly` | `true` | Record an undo step; the result names the inverse call | any caller | the verb declares `supports(reversibly)` |
| `within` | `{ "ms": N }` | Give up after N ms with `timed-out` | any caller | read-only verbs, or `supports(within)` |
| `in_background` | `true` | Start as a job and answer the handle | any caller | a `BackgroundRunner` the host installed |
| `every` | duration string | Run again on this interval until cancelled; answers the job handle of the routine | any caller | the verb declares `supports(every)`, and a `BackgroundRunner` that can `repeat` |

### Rules

1. **Universal or declared, never silent.** `tentatively`, `with_review`
   and `once` work on every verb with no declaration. The others need
   either a declaration on the verb (`supports(reversibly, within)`) or a
   hook the host installed. An adverb a verb cannot honor is refused
   `unsupported-adverb`, naming the adverb and the verb. A call never
   proceeds as if an adverb had been honored when it was not.
2. **`tentatively` is honest about its depth.** For every verb it answers,
   without running the handler, what the pipeline knows statically: the
   verb, its safety class, its declared reads, writes and reach, the
   caller, and the policy decision the real call would get (`run`,
   `review`, `deny`, `conflict`). A verb that declares
   `supports(tentatively)` is run with the adverb in its call context and
   must then write nothing; the store spy in the suite pins that. The
   envelope says which depth answered: `"tentative": "static"` or
   `"tentative": "simulated"`.
   A call made inside a dry run is part of it (plan-koine-trust TR-D2):
   it inherits `tentatively`, so a write verb a simulating verb calls
   answers statically instead of running, while a read-only verb it calls
   runs (it changes nothing and its answer is needed), still inside the
   dry run for whatever it calls in turn. Only `tentatively` is inherited.
   **Rehearsal** (TR-D3): when the policy would run the call, the verb
   stays in the process (no external reach, not `External`) and the host
   can make an overlay (`koine_store::rehearse`, installed with the process
   store), the real handler runs against a copy-on-write overlay of the
   store: reads see the overlay over the store, writes land in memory and
   are dropped. The answer is the verb's real result with
   `"tentative": "rehearsed"` and `"changes"` (`inserted`, `updated` field
   by field, `deleted`, `operations`). Calls inside a rehearsal run on the
   overlay too; one that leaves the process answers statically and the
   rehearsal falls back to the static answer, with a `reason`. Every store
   backend refuses a write made during a dry run anywhere but the overlay
   (`impress_item::ambient::check_dry_run_write`), so a verb that holds its
   own store cannot write either; that refusal also falls back to the
   static answer. `koine-host`'s effects test rehearses every Tier A example
   and checks the store saw no write.
3. **`reversibly` produces an inverse call, not a promise.** A supporting
   verb records its inverse during the call
   (`context::note_inverse(verb, args)`); the pipeline puts it on the
   envelope as `"undo": { "verb": …, "args": … }` and on the audit row. A
   read-only verb is reversible by construction and answers `"undo":
   null`. A verb that supports the adverb but recorded no inverse answers
   `effect-failed`: the undo would have been a lie.
4. **`once` is keyed by caller, verb and key, and compares the arguments**
   (plan-koine-trust TR-D1, after the IETF Idempotency-Key draft). The
   first call claims the key and the answer is stored with a sha256
   fingerprint of the arguments (canonical JSON, the `adverbs` key
   removed). A repeat with the same arguments answers the stored envelope
   with `"replayed": true`; with other arguments it answers
   `idempotency-mismatch` (422); while the first call still runs it answers
   `idempotency-in-flight` (409). Failures are stored like successes,
   except the retryable codes (`conflict`, `store-unavailable`,
   `host-unavailable`, `timed-out`), which release the key so a retry runs.
   An answer is kept 24 hours (`IMPRESS_ONCE_TTL`, seconds). The pipeline
   keeps a bounded table per process; koine-host installs a durable
   `IdempotencyStore` (`koine_store::once`), whose rows hold the key's
   sha256, never the key. A verb task's attempts share the key
   `task:<id>` unless the filer named one, so a retry never runs the verb
   twice. The key is never stored by value in the audit row.
5. **`within` cancels only what can be cancelled.** On a read-only verb
   the handler future is dropped at the deadline and the call answers
   `timed-out`. On any other verb the adverb is refused unless the verb
   declares `supports(within)`, which is its promise that dropping the
   future mid-flight leaves the store consistent.
6. **`in_background` is the host's.** The pipeline hands the prepared call
   to the installed `BackgroundRunner` and answers the `JobStarted` shape
   of `impress-service-core::job`. With no runner installed the adverb is
   refused `unsupported-adverb`. The runner is where the job row, the
   progress, the emitted lines (`job::emit`, docs/capability.md § A verb
   that emits) and the cancel live; the pipeline stays store-free.
7. **`every` is `in_background`, repeated.** Its value is a duration
   string, an unsigned integer and one unit: `"2s"`, `"10m"`, `"1h"`,
   `"2d"`. Seconds at least 1 (`"500ms"` is refused), days at most (365). A
   bad string is `invalid-argument` saying what it takes. It is never
   universal: a verb that does not declare `supports(every)` is refused
   `unsupported-adverb` like any other undeclared adverb, and so is a call
   in a process with no runner installed. It implies the background path:
   the pipeline hands the call, with `every` and `in_background` cleared,
   to `BackgroundRunner::repeat(verb, call, interval)`, which answers the
   `JobStarted` shape of the routine (a runner that cannot repeat answers
   `unsupported-adverb`, "this runner cannot repeat"). Each run goes
   through the pipeline again, so policy and audit see every one; `cancel`
   stops the routine. What the host's runner keeps (`runs`, `last_result`,
   `last_finished`, re-arming after a restart) is the host's
   (`docs/plan-koine-live.md` L-D1).
8. **Adverbs are on the record.** The span and the audit row carry the
   adverbs a call used (the names, `within.ms` and the `every` string;
   never the `once` key's value). The call log can therefore say that a change was made
   tentatively, under review, or in the background.
9. **The generated form shows the adverbs the verb accepts.** The verb
   surface adds one toggle per universal adverb and per declared boolean
   one, and the Run action passes them. A frontend holds no logic for them.

### Order in the pipeline

The eight layers of ADR-0034 D2 become nine. `adverbs` is parsed right
after identity and before strict args, so a misspelt adverb is refused
before anything else happens. `tentatively` is answered after policy has
*decided* but before it has *acted*: the answer reports the decision
without queuing, denying or taking a write lease. `once` consults its
table just before invoke and records just after. `with_review` turns the
policy decision into `Review` before it is acted on. `in_background`
(and `every`) replaces invoke with the runner. `within` wraps invoke. `reversibly`
reads the inverse the handler recorded, in the envelope layer.

### Known limits (2026-10-11)

- The generated form offers only the boolean adverbs. `once`, `within` and
  `every` wait on a reduce-time rule that drops an empty adverb, because the
  form sends every template it offers and the pipeline refuses an empty key,
  a zero deadline or an empty interval. A routine is started from a button
  whose `call` action carries `"adverbs": {"every": "2s"}`.
- The in-flight claim is per process: two processes sharing a store can
  both run a first call with the same `once` key. A durable answer is seen
  by both once it is stored.
- A tentative call on an app that is not running is answered unavailable,
  not tentatively: reachability is checked before policy.
- A method cannot declare `supports()` to clear a non-empty service
  default; an empty declaration inherits it.
- A runtime provider declares no adverb support; it honors the universal
  three only.

## Consent: pending calls

A call the policy holds for a person (plan-koine-trust TR-D4, TR-D5) is not
lost. With a review queue installed (`koine_tasks::review`, which koine-host
installs), it is kept as a `koine/pending-call@1.0.0` item with a
`review-request@1.0.0` on it, and the caller gets `code: "review-pending"`,
`status: "input_required"` and the `pending_id`. `review-service_list`,
`_show`, `_approve` and `_reject` read and decide them; only the person or
their app may decide. Approving runs the call at once, as the caller that
asked, under a one-use approval the policy honours for that verb with
those arguments and for the calls it makes; a clock-guarded move from
`pending` to `running` keeps two approvers from running it twice. An
approval made elsewhere (a review resolved through another app) is run by
the host's sweep, and a pending call expires after 7 days
(`KOINE_REVIEW_EXPIRY_DAYS`).

What holds a call (TR-D6) is the consent policy, `ConsentPolicy`
(`IMPRESS_VERB_POLICY=consent`, or `policy::install`). For a call that is
not a read, in order: a standing rule that rejects it refuses it (code
`rejected`); the session's risk may hold it (TR-D11); a standing rule that
allows it lets it run; the verb's own review check,
`#[impress_method(review = f)]`, decides from the arguments (deleting a draft
needs no review, deleting a cited paper does); otherwise its impact —
`impact = "…"`, else low for read-only, medium for mutating, high for
destructive and external — is compared with the caller's threshold (agents
from medium, the `tasks` agent from high, the person and their app never;
`policy::Thresholds`). The answer and the pending call say why (`reason`).

A pending call names the records it acts on (`targets`: the argument
`targets = "…"` names, or the effects' target argument, with each record's
kind and title) and the verbs a person may choose instead
(`alternatives = […]`). `review-service_approve` takes `only` (a subset of
the targets) and `instead` (one of the alternatives), and `remember` writes
a standing rule (`koine/consent-rule@1.0.0`): allow this verb from this
caller for 30 days, or (on reject) refuse it. A rule records the verb's
fingerprint; when the verb changes, the rule no longer applies and the
person is asked again. `review-service_rules` lists rules (and which are
stale); `_revoke-rule` revokes one.

Where the person decides (TR-D8): the kit ships a **Review** surface
(`koine-packages/surfaces/review.surface.json`, installed where the review
verbs are linked) — the calls waiting, the question, the records each acts
on and its arguments, approve or reject once or for good, what was decided
recently, and the standing rules to revoke. imbib's and imprint's review
lists show the same `review-request` rows. A person running a destructive or
external verb from a generated form previews it first (`tentatively`, so
rehearsed where it stays in the process) and the Run button appears once the
preview is there; there is no "I understand" toggle to click through.

### Sessions and risk

Every call can carry a session (plan-koine-trust TR-D10): `Call::session`,
inherited by every call made inside it and kept on the call record. The MCP
host uses the client's `Mcp-Session-Id`, else one per agent per hour;
koine-host's `/api/verb` reads `X-Koine-Session`, else one per hour; the
model executor uses the conversation (`ai:<conversation>`); a verb task runs
in the session it was filed in, and an approved pending call in the session
that asked for it.

A verb declares its risk (TR-D11): `risk(private)` when it reads the
person's data, `risk(untrusted)` when it takes in content they did not write
(the web, a fetched package, an imported document, a model's output).
Sending data out (`egress`) is never declared: every verb that reaches out
of the process carries it, and such a verb is `external`
(`report::reach_class_disagreements`, checked by koine-host's effects
test). `tools/list` shows it as `_meta.risk`; the skill lists it.

The consent policy keeps the "Rule of Two": the pipeline records what each
session has run, and once an agent's session holds two of private data,
untrusted content and egress, a call that would add the third waits for a
person; once it holds all three, every write and every egress does. A
session's record lapses an hour after its last call. The person and their
app are never held for it.

Every answer that is not a result carries a `status` in the agent
protocols' words: `input_required` (held for a person), `auth_required`
(`missing-secret`), `rejected` (the policy refused; code `rejected`). A verb
task whose call needs a secret waits on a review instead of failing.

## What pins this

| Rule | Test |
|---|---|
| Adjective registry: unique names, every built-in lowers, listing round-trips | `impress-pane-query` unit tests |
| Expansion refuses unknown, wrong-kind, ill-typed adjectives with a typed error | `impress-pane-query` unit tests |
| Unknown adverb key is `invalid-argument`; unsupported adverb is `unsupported-adverb` | `impress-service-core::pipeline` tests |
| A tentative call never runs an undeclared handler, and reports the policy decision | `impress-service-core::pipeline` tests |
| `once` replays, `within` times out a read-only verb, `in_background` refuses without a runner | `impress-service-core::pipeline` tests |
| `every` parses `2s`/`10m`/`1h`/`2d` and refuses `500ms`, `0s`, over a year and a non-string; it needs the verb's declaration and a runner, and reaches `repeat` with the interval and neither adverb | `impress-service-core` adverb and pipeline tests; `koine-host` `tests/host.rs` (`every_*`) |
| The macro refuses `supports(x)` for an unknown adverb | `impress-service-macros` compile-fail test |
| A surface `call` action's `adverbs` reach the effect unchanged | `impress-surface` reduce tests |
| A scenario `call` step's `adverbs` reach the caller | `impress-scenario` tests |
| Every adverb key a surface example uses is in the pipeline's vocabulary | `koine` crate test |
| The verb surface offers exactly the adverbs the descriptor accepts | `impress-verb-surface` tests |
| The macro refuses a `returns_kind` that is not a kind id, and accepts one that is | `impress-service-macros` compile-fail test (`returns_kind_not_a_kind`) and pass case |
| A declared `returns_kind` is a well-formed kind id, reaches `_meta` and labels the result table | `koine` crate test |
| The macro refuses `rows(…)` without `returns_kind` or without `id`, and a `secrets` name that is not a name; it accepts the well-formed forms | `impress-service-macros` compile-fail tests (`rows_without_returns_kind`, `rows_missing_id`, `secret_not_a_name`) and pass case (`secrets_and_rows`) |
| A `rows(…)` field the output schema lacks fails `row_schema_check` and the Tier A report; declared rows reach `_meta.rows`, the table's `id_field` and `state.selected` | `impress-service-core` descriptor and Tier A tests, `impress-verb-surface` tests, `koine` crate test |
| A verb whose declared secret is absent is refused `missing-secret` before the handler and runs once it is set; the form warns | `impress-service-core::pipeline` test, `impress-verb-surface` test |
