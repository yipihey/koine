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
| `once` | key string | Idempotent under retry: a repeat with the same key answers the first result | any caller, any verb | nothing in-process; a host may install a durable `IdempotencyStore` |
| `reversibly` | `true` | Record an undo step; the result names the inverse call | any caller | the verb declares `supports(reversibly)` |
| `within` | `{ "ms": N }` | Give up after N ms with `timed-out` | any caller | read-only verbs, or `supports(within)` |
| `in_background` | `true` | Start as a job and answer the handle | any caller | a `BackgroundRunner` the host installed |

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
3. **`reversibly` produces an inverse call, not a promise.** A supporting
   verb records its inverse during the call
   (`context::note_inverse(verb, args)`); the pipeline puts it on the
   envelope as `"undo": { "verb": …, "args": … }` and on the audit row. A
   read-only verb is reversible by construction and answers `"undo":
   null`. A verb that supports the adverb but recorded no inverse answers
   `effect-failed`: the undo would have been a lie.
4. **`once` is keyed by caller and key.** The pipeline keeps a bounded
   table (key → envelope) per process; a repeat answers the stored
   envelope with `"replayed": true`. A host that wants it to survive a
   restart installs an `IdempotencyStore`. The key is never stored by
   value in the audit row.
5. **`within` cancels only what can be cancelled.** On a read-only verb
   the handler future is dropped at the deadline and the call answers
   `timed-out`. On any other verb the adverb is refused unless the verb
   declares `supports(within)`, which is its promise that dropping the
   future mid-flight leaves the store consistent.
6. **`in_background` is the host's.** The pipeline hands the prepared call
   to the installed `BackgroundRunner` and answers the `JobStarted` shape
   of `impress-service-core::job`. With no runner installed the adverb is
   refused `unsupported-adverb`. The runner is where the job row, the
   progress and the cancel live; the pipeline stays store-free.
7. **Adverbs are on the record.** The span and the audit row carry the
   adverbs a call used (the names, and `within.ms`; never the `once` key's
   value). The call log can therefore say that a change was made
   tentatively, under review, or in the background.
8. **The generated form shows the adverbs the verb accepts.** The verb
   surface adds one toggle per universal adverb and per declared one, and
   the Run action passes them. A frontend holds no logic for them.

### Order in the pipeline

The eight layers of ADR-0034 D2 become nine. `adverbs` is parsed right
after identity and before strict args, so a misspelt adverb is refused
before anything else happens. `tentatively` is answered after policy has
*decided* but before it has *acted*: the answer reports the decision
without queuing, denying or taking a write lease. `once` consults its
table just before invoke and records just after. `with_review` turns the
policy decision into `Review` before it is acted on. `in_background`
replaces invoke with the runner. `within` wraps invoke. `reversibly`
reads the inverse the handler recorded, in the envelope layer.

## What pins this

| Rule | Test |
|---|---|
| Adjective registry: unique names, every built-in lowers, listing round-trips | `impress-pane-query` unit tests |
| Expansion refuses unknown, wrong-kind, ill-typed adjectives with a typed error | `impress-pane-query` unit tests |
| Unknown adverb key is `invalid-argument`; unsupported adverb is `unsupported-adverb` | `impress-service-core::pipeline` tests |
| A tentative call never runs an undeclared handler, and reports the policy decision | `impress-service-core::pipeline` tests |
| `once` replays, `within` times out a read-only verb, `in_background` refuses without a runner | `impress-service-core::pipeline` tests |
| The macro refuses `supports(x)` for an unknown adverb | `impress-service-macros` compile-fail test |
| A surface `call` action's `adverbs` reach the effect unchanged | `impress-surface` reduce tests |
| A scenario `call` step's `adverbs` reach the caller | `impress-scenario` tests |
| Every adverb key a surface example uses is in the pipeline's vocabulary | `koine` crate test |
| The verb surface offers exactly the adverbs the descriptor accepts | `impress-verb-surface` tests |
