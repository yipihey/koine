# Plan: trust — honest adverbs, a closed review loop, session risk, kinds that evolve, App Intents

**Builds on:** research-prior-art.md §6 (the five recommendations this plan
implements), grammar.md § Adverbs and Consent, plan-koine-ai-tasks.md (the
task kernel and its review requests), plan-koine-store.md (the port and the
conformance suite), plan-koine-research-os.md (the desk, packages), impress-
apps ADR-0034 (pipeline and transport) and ADR-0036 (effects).

## Why

Koine's bet is that people and agents can share one store because every
verb says what it does and one pipeline decides whether it may. The survey
found that the bet is right and that koine is ahead of its relatives on it,
and the code survey that followed found five places where the pipeline does
not yet keep the promise the grammar makes:

1. **The adverbs are not honest yet.**
   - `once` keys on `(caller, key)` only, without the verb, so the same key on
     a second verb replays the first verb's answer (pipeline/mod.rs:532).
     There is no argument fingerprint and no in-flight guard, failures are
     never stored, the table is an in-memory FIFO of 1024, and no host
     installs a durable store.
   - `tentatively` is not inherited by nested calls (`prepare_inner`,
     mod.rs:422-444 inherits caller, trace and store but not adverbs).
   - No production verb declares `supports(tentatively)`; every dry run is
     the static answer. Nothing at the store refuses a write under a dry run.
2. **The review loop is open.**
   - `Policy` and `ReviewQueue` exist (pipeline/policy.rs), but no production
     code installs a queue. A `review-pending` answer persists nothing; no
     verb or route lists, approves or re-runs it.
   - A verb task that meets `review-pending` or `missing-secret` fails
     permanently (koine-tasks verb.rs:33).
   - The generated form guards destructive verbs with "I understand, run it
     anyway" — the click-through pattern the survey warns about.
   - The working human checkpoint is the task kernel's `review-request@1.0.0`
     (with UIs in imbib and imprint and a 7-day expiry in impel-taskd).
3. **Nothing tracks what an agent session has touched.**
   - No identifier groups one agent session's calls: MCP is stateless, the AI
     tool loop does not pass its run id into the pipeline.
   - `Policy::decide` sees only the caller and the descriptor.
   - Effects say what a verb reads and writes and where it reaches, but not
     whether it reads private data or takes in untrusted content; and reach
     is not tied to the safety class (package search and fetch reach the
     network yet are `read_only`).
4. **Kinds have no evolution rules.**
   - Kind field schemas are Rust in impress-core, never checked on write;
     koine has none. `@1.0.0` in a kind name is spelling, not semantics.
   - Payloads round-trip opaquely, but envelope fields a build does not know
     are dropped (dir rows, the sync envelope), and typed read-modify-writes
     of layouts and surface specs strip unknown nested keys — and whole-record
     last-write-wins sync spreads an older build's stripped copy to every
     device.
   - `Item.version` exists and is filled by three writers, inconsistently.
5. **The Apple apps' intents bypass the verbs.**
   - imbib, imprint, implore, impart and impel have hand-written App Intents
     and entities that call their own adapters, not koine verbs; implore's
     and impart's service locators are never assigned, so their queries are
     empty. Nothing sets `appEntityIdentifier`; nothing uses
     `IndexedEntity`.
   - Every app already has a generic `dispatch_verb(name, args, caller)` FFI,
     and impress-apps has `gen-verb-docs` (descriptors → committed files,
     checked by regenerate-and-diff) as the generator pattern to copy.

## Decisions

### TR-D1. `once` is an idempotency key in the IETF draft's sense

- The key is `(caller author, verb name, key)`, and the entry stores a
  fingerprint of the arguments (sha256 of canonical JSON, adverbs removed).
- The same key with different arguments answers `idempotency-mismatch`
  (HTTP 422) and does not run.
- The first call reserves the key before running; a second call while it
  runs answers `idempotency-in-flight` (409).
- A stored answer is the envelope, failures included, except the retryable
  ones (`conflict`, `store-unavailable`, `host-unavailable`, `timed-out`):
  those release the key so a retry can run.
- Retention is 24 hours by default (`IMPRESS_ONCE_TTL`), documented in
  grammar.md.
- Every host installs a durable `IdempotencyStore`: rows of kind
  `core/once@1.0.0`, `ephemeral`, excluded from sync. The in-memory table
  stays as the fallback for a process with no store.
- A verb task derives its key from its task id (`task:<id>`) unless the
  filer gave one, so a retried attempt never runs twice.

### TR-D2. Adverbs and the verb's fingerprint travel with the call

- A nested call inherits the outer call's `tentatively`, `within` (as the
  remaining deadline) and `with_review`; `once`, `in_background` and `every`
  stay with the call that named them. The one place is `prepare_inner`.
- `VerbDescriptor::fingerprint()` is a sha256 of the descriptor's name,
  input and output schemas, safety, effects and risk. It is written on
  every call record, shown in `tools/list` `_meta`, the skill and
  `describe`. TR-D9 binds grants to it; replay and staleness use it later.

### TR-D3. A dry run is enforced at the store, and can rehearse

Three answers to `tentatively`, chosen by the pipeline:

- **Rehearse** (new). The handler runs against an overlay store: reads see
  the overlay over the base, writes land in the overlay, and the overlay is
  dropped at the end. The answer is the verb's real result, tagged
  `"tentative":"rehearsed"`, plus the would-be diff (items inserted, updated
  with field-level before/after, deleted; operations that would be written).
  - Chosen when the verb has no external reach (`fs` alone is allowed) and
    its class is not `External`.
  - The overlay is `koine-store-overlay`, an `AttributedStore` wrapper over
    any port, conformance-tested; it is set as the call's scoped store, so
    `acquire()` and `CallStore` hand it to the verb.
- **Simulate**: a verb that declares `supports(tentatively)` keeps its own
  simulation (today only tests).
- **Static**: everything else keeps today's answer, with `reason` saying why
  it could not rehearse (external reach; a store that cannot be
  overlaid).

The store also refuses, as a backstop: an ambient dry-run flag, set by the
pipeline beside the ambient batch (impress-item `ambient.rs`), makes every
backend's write path answer `dry-run-write`. A verb that holds an explicit
store or downcasts to `SqliteItemStore` therefore cannot write during a
rehearsal; the pipeline turns that refusal into the static answer.

- A dry run whose call tree reaches an external verb is refused at that
  nested call (inherited flag), and the outer answer degrades to static.
- The notebook sink writes outside any call and into the base store; it
  never sees the overlay.
- The effects spy gains the missing pin (grammar.md rule 2): every Tier A
  example rehearsed leaves the base store byte-for-byte unchanged.

### TR-D4. A pending call is a verb task waiting on a review request

The review loop closes on the task kernel rather than on a new mechanism:

- `koine-tasks` provides the production `ReviewQueue`. `enqueue` files a
  `koine.verb` task (verb, arguments, caller, session) and opens a
  `review-request@1.0.0` on it with the question, the reason, the impact,
  the targets (TR-D7) and any alternatives. The task stays suspended until
  the request is resolved — the `AwaitHumanResponse` path that already
  exists.
- `approved` re-runs the call as the original caller with a one-use approval
  token, so policy does not ask again. `rejected` closes the task as
  `rejected`. Requests expire after 7 days (koine-tasks takes over
  impel-taskd's sweep).
- Hosts install it: koine-host, and impress-apps' service hosts.
- `review-service_*` verbs (in koine-tasks, `service` feature): `list`
  (pending, grouped by session), `show`, `approve {id, remember?}`,
  `reject {id, remember?}`, `approve-session {session}`. Only a native
  `Person` or `App` caller may approve or reject (the rule
  `provider-service_set-trusted` already uses).
- Secrets are never stored in a pending call (they are never arguments).

### TR-D5. Pending is a state, not an error

The envelope keeps `ok:false` and its code, and gains a `status` that maps
onto the agent protocols:

| Situation | `code` | `status` | MCP / A2A |
|---|---|---|---|
| Waiting for a person | `review-pending` (+ `pending_id`) | `input_required` | `input_required` / `INPUT_REQUIRED` |
| A secret is missing | `missing-secret` | `auth_required` | `AUTH_REQUIRED` |
| Policy refused | `rejected` (was `forbidden`) | `rejected` | `REJECTED` |
| Failed | as today | `failed` | `failed` |

- The skill tells agents: `input_required` — tell the person, do not retry,
  poll `review-service_show {pending_id}`; `auth_required` — ask the person
  to set the secret; `rejected` — do not retry.
- A verb task whose call answers `auth_required` suspends like a review
  instead of failing permanently.
- MCP tasks (protocol 2025-11-25): koine-mcp-host negotiates 2025-11-25
  (or 2025-06-18; never 2025-03-26, whose batching it refuses), declares
  `tasks` (`list`, `cancel`, `requests.tools.call`) and marks every tool
  `execution.taskSupport: "optional"`. A `tools/call` with `task` answers a
  `CreateTaskResult` and runs on its own thread; a held call is a task in
  `input_required` whose `tasks/get` reads the pending call
  (`review-service_show`), so it completes with the approved call's answer
  or fails when the person rejects it or it expires. `tasks/result` blocks
  until terminal (stdio answers it on its own thread); `tasks/cancel`
  withdraws a held call. Tasks are bound to their caller and kept for their
  ttl (at most a day). `structuredContent` still carries the status for
  clients without tasks.
- A missing secret (`auth_required`, now naming `missing_secrets`) is a
  `-32042` URL elicitation for a client that declared `elicitation.url`,
  pointing at `HostConfig::secret_url`; koine-host serves that page at
  `/secret/<name>` (the loopback token only for a loopback peer), which puts
  the value straight to `/api/secret/<name>`.

### TR-D6. Review is decided by impact, the caller, the arguments and rules

`Policy::decide` takes one `PolicyInput`: caller, verb facts, arguments, the
call's session (TR-D10) and a read handle on the store. The built-in policy
decides, in order:

1. A standing **rule** (TR-D9) that matches: allow or deny.
2. **Session risk** (TR-D11): escalate to review.
3. The verb's **review hook**, if it declares one:
   `#[impress_method(review = path::to_fn)]`, a
   `fn(&Value, &dyn ItemStore) -> ReviewNeed` (`None`, or `Review { reason,
   targets }`). Deleting a draft needs no review; deleting a cited paper
   does.
4. **Impact against the caller's threshold.** Impact is `low`, `medium` or
   `high`, defaulting from the class (read-only low, mutating medium,
   destructive and external high), overridable with
   `#[impress_method(impact = "…")]`. Thresholds are a settings item
   (`core/consent@1.0.0`): a person never by default (opt in per verb with
   `with_review`), agents at `medium` (today's `review-agent-writes`), the
   `tasks` agent at `high`. `IMPRESS_VERB_POLICY` keeps working as a preset.

### TR-D7. A review names what it touches and what else could be done

- A verb whose effects name a target argument (`Kind::Target(arg)`,
  `Children(arg)`) has its targets resolved into the review request: ids,
  kinds and titles (the row schema's title), so the person sees "these 12
  papers", not a verb name.
- A verb declares an id-list argument with `targets(arg)`; the person may
  approve a subset, and the call re-runs with only those ids.
- `#[impress_method(alternatives = ["imbib-service_archive"])]` offers other
  verbs on the same arguments (App Intents' `requestChoice`); choosing one
  files that verb instead, under the same approval.
- The generated verb form drops "I understand, run it anyway": a person
  running a high-impact verb sees the same target list and confirms it.

### TR-D8. The review queue is visible everywhere the person is

- koine-web gets a Review surface (built-in package, like the notebook's):
  pending calls grouped by session, targets, approve / reject / remember.
- imbib's `ReviewQueueListView` and imprint's throughline already list
  `review-request` items; they gain the pending-call fields.
- The agent desk shows a session's pending calls beside its request.

### TR-D9. "Always" is a rule the person can see and revoke

- Approving or rejecting with `remember` writes a `core/consent-rule@1.0.0`
  item: verb (or service pattern), caller pattern, optional kind, decision,
  expiry (default: none for deny, 30 days for allow), the verb's descriptor
  fingerprint, who made it and from which pending call.
- A rule whose fingerprint no longer matches the verb is ignored and flagged
  (the MCP rug-pull defence): a verb whose effects changed asks again.
- `review-service_rules`, `_revoke-rule`; rules are listed in the Review
  surface. Rules are excluded from sync until device-scoped rules are
  designed.

### TR-D10. Every call carries a session

- `Call`, `CallContext` and `TransportContext` gain `session: Option<String>`,
  inherited by nested calls like the trace.
- Who sets it:
  - koine-mcp-host and koine-host `/mcp`: one per MCP connection
    (`Mcp-Session-Id` when the client sends one, else minted at
    `initialize`).
  - koine-host `/api/verb`: the `X-Koine-Session` header; without it, one
    session per bearer token per hour.
  - koine-ai: the agent run id; the desk: the request id; a verb task: the
    session of whoever filed it.
  - Native apps: one per app launch for `App`, none for `Person`.
- The call record stores it; `notebook-service_calls` can filter by it.

### TR-D11. Risk is declared, and a session that holds all three escalates

- Effects gain `risk`: `private` (reads the person's data), `untrusted`
  (takes in content the person did not write: the web, a fetched package, an
  imported PDF or email, a model's output) and `egress` (sends anything out
  of the process).
  - `egress` is implied by any external reach (network, provider, app,
    device, subprocess) and cannot be declared away.
  - `private` and `untrusted` are declared:
    `#[impress_method(risk(private, untrusted))]`, or at service level.
  - A verb whose reads are `Any` and declares no risk is listed by a Tier A
    report until it is marked or exempted with a reason.
- Reach and class agree: a verb with external reach must be `External`. The
  macro refuses the mismatch; the package verbs that are `read_only` today
  become `external`.
- The session ledger (in policy, beside `IN_FLIGHT`) records the union of
  risk each session has *run* (not merely asked), with a one-hour idle
  expiry, mirrored into the call record.
- The rule (the "lethal trifecta" / Meta's "Rule of Two"): once an
  agent session holds two of the three, a call that would add the third is
  escalated to review; once it holds all three, every write and every egress
  call is. Person callers are not escalated.
- `_meta` in `tools/list` and the skill show each verb's risk.
- Provenance-tagged values and plan-then-execute for the built-in model
  (CaMeL) are the follow-up; the session ledger is the substrate they need.

### TR-D12. Kinds evolve by AT Protocol's Lexicon rules

- koine gains `KindSchema` (fields with name, type, required; edges;
  inherits), ported from impress-core's `Schema`, and a registry. Schemas are
  items (`core/kind-schema@1.0.0`), so packages ship them and stores sync
  them; impress-core's Rust schemas register through it.
- Compatibility is a function, `compatible(old, new) -> Result<(), Breaks>`:
  only optional fields may be added; required fields stay; types,
  enumerations and limits do not narrow. The registry refuses an
  incompatible schema under the same name; a breaking change is a new name
  (`task@2.0.0` — the existing versioned spelling, now with meaning).
- Every write stamps the kind schema version it was written under: on
  items, the existing `Item.version`, filled by the port's write helpers
  from the registry; on operations, an `op_schema_version` payload key in
  `build_operation_payload`.
- Validation on write starts in warn mode (counted in
  `notebook-service_health`), and is enforced per kind once its writers are
  clean.
- Cambria-style lenses are not built now; the stamps keep them possible.

### TR-D13. What a build does not understand, it keeps

- `Item` gains `extra: BTreeMap<String, Value>` (`#[serde(flatten)]`) for
  envelope fields a build does not know; the dir backend and the sync
  envelope round-trip it, and SQLite keeps it in an `extra` JSON column.
- Typed read-modify-writes merge rather than replace: layouts, presets and
  surface specs carry `#[serde(flatten)] extra` at every level that is
  written back, and unknown tile or node variants are kept as raw JSON, so
  an older build never strips a newer build's fields — which today
  whole-record sync spreads to every device.
- Items of a kind the build does not know are read, listed, synced and
  written back untouched; strict `parse_schema_ref` refusals in imbib-core
  and koine-memory become skips with a count.
- New conformance cases, run by every backend including SQLite:
  `unknown_payload_fields_survive`, `unknown_envelope_fields_survive`,
  `unknown_kind_round_trips`, `item_before_its_kind_schema`,
  `version_is_stamped_on_write`, `op_schema_version_is_stamped`.

### TR-D14. App Intents are generated from the inventory

- A verb opts in: `#[impress_method(intent(title = "Add Papers", phrase =
  "Add papers to ${applicationName}"))]`. Opt-in, because every verb in
  Shortcuts would bury the useful ones.
- A kind becomes an `AppEntity` when the host's manifest names, for it, a
  lookup verb by ids and a search verb by string; the entity's id and title
  come from the kind's row schema.
- `koine-app-intents` (a library) renders Swift from descriptors and the
  manifest:
  - an `AppIntent` per opted-in verb, with `@Parameter`s from the input
    schema (strings, numbers, booleans, enums, dates, entities for target
    arguments);
  - `perform()` dispatches through the existing
    `dispatch_verb_async(name, args, caller)` with caller
    `{"kind":"person","name":"app-intents"}` and a fresh trace;
  - high impact → `requestConfirmation`, listing targets; alternatives →
    `requestChoice`; `review-pending` → a dialog that names the pending
    review;
  - entities, `EntityQuery`/`EntityStringQuery`, and an
    `AppShortcutsProvider`.
- impress-apps gets a `gen-app-intents` binary (the `gen-verb-docs`
  pattern): it writes committed Swift into each app's Core package under
  `Generated/AppIntents/`, with `AppIntentsPackage` conformances so
  Shortcuts finds intents in SwiftPM packages, and
  `scripts/check-app-intents.sh` regenerates and fails on a diff.
- Pointing reaches Siri: a Swift `PointingActivity` observes the key
  window's focused pane and its channel's selection and publishes an
  `NSUserActivity` with `appEntityIdentifier`; generated row views carry
  `.appEntityIdentifier`. Spotlight `IndexedEntity` follows.
- Hand-written intents are retired as their generated replacements land;
  implore's and impart's unassigned locators go with them.

### TR-D15. Both repositories stay green at every step

As KM-D7: impress-apps builds koine from `../koine` while a package is in
flight and pins a pushed rev when it lands; both full gates pass at every
commit, including impress-apps' pre-push xcframework build.

## Work packages

In order; a package lists what it needs. TR1–TR3 touch the pipeline and
must go in sequence; TR7–TR8 (store) and TR9 (generator) can run beside
them in their own sessions.

| WP | What | Needs | Lands in |
|---|---|---|---|
| **TR1** | `once` per TR-D1: verb in the key, argument fingerprint, in-flight reservation, stored failures, TTL, durable `core/once` store installed by koine-host and impress hosts, task-derived keys | — | impress-service-core, koine-host, koine-tasks; impress hosts |
| **TR2** | Adverb inheritance and descriptor fingerprints per TR-D2; fingerprint on call records, `_meta`, skill | — | impress-service-core, koine-notebook, koine-agent |
| **TR3** | Rehearsal per TR-D3: `koine-store-overlay` (+ conformance), ambient dry-run flag in every backend (memory, dir, SQLite), the rehearse disposition and diff, the spy pin | TR2 | impress-item, new crate, backends, pipeline, koine-host tests; impress-core SQLite |
| **TR4** | The review loop per TR-D4/D5: the task-kernel `ReviewQueue`, approval tokens, `review-service_*`, statuses and codes, `auth_required` suspension, expiry, hosts install it, skill text | TR2 | koine-tasks, pipeline, koine-host, koine-mcp-host, koine-agent; impress hosts |
| **TR5** | Consent decisions per TR-D6/D7/D9: `PolicyInput`, impact and thresholds, the review hook, targets and subsets, alternatives, consent rules bound to fingerprints, the form change | TR4 | macros, descriptor, policy, koine-tasks, impress-verb-surface |
| **TR6** | Review everywhere per TR-D8: koine-web Review surface (built-in package), imbib/imprint review lists, desk | TR4 | koine-web, koine-packages; impress-apps Swift |
| **TR7** | Sessions and risk per TR-D10/D11: `session` on calls and transports, `risk` in effects and the macro, reach/class agreement (package verbs), ledger and escalation, Tier A risk report, `_meta` and skill; declare risk on koine's and impress-apps' verbs | TR5 | pipeline, macros, mcp-host, koine-host, koine-ai, koine-agent, all services |
| **TR8** | Kind schemas per TR-D12: `KindSchema`, registry, `compatible`, schema items, version stamps, warn-mode validation; impress-core schemas through it | — | impress-item (or new `koine-kinds`), backends; impress-core |
| **TR9** | Keeping what is not understood per TR-D13: `Item.extra`, dir/sync/SQLite round-trip, `extra` on layout/surface specs and presets, unknown-kind tolerance, the six conformance cases | TR8 for the stamp cases | impress-item, koine-store-*, conformance, impress-layout, impress-surface; impress-core sync |
| **TR10** | `koine-app-intents` and the `intent(...)` attribute per TR-D14; render tests against fixture descriptors | TR5 (impact, alternatives) | macros, descriptor, new crate |
| **TR11** | impress-apps adoption: `returns_kind`/`rows` on imbib, imprint and implore verbs; manifest lookup verbs; `gen-app-intents` + check script; imbib first (papers, collections), then imprint (manuscripts) and implore (figures); `PointingActivity`; retire hand-written intents | TR10 | impress-apps |

## What pins this

| Promise | Test |
|---|---|
| `once` never replays another verb's or other arguments' answer | pipeline tests: key on two verbs; same key, different args → mismatch; concurrent first calls → one runs |
| A retried task never runs its verb twice | koine-tasks: a flaky executor retried, the verb's counter is 1 |
| A rehearsal changes nothing | koine-host effects test: every Tier A example rehearsed, base store unchanged; nested call inherits the flag |
| The overlay is a store | koine-store-overlay runs the conformance suite |
| A pending call is resumable and asked once | koine-tasks: enqueue → approve → runs once as the original caller; reject → `rejected`; expiry |
| A remembered rule stops applying when the verb changes | policy test with two fingerprints |
| A session holding all three risks is escalated | policy test over a scripted session; e2e: an agent fetches untrusted content, reads private data, then tries to send → `input_required` |
| Reach and class agree | macro UI test (compile-fail) and a Tier A check over the inventory |
| Kinds evolve compatibly | `compatible()` unit tests; registry refuses a breaking change |
| Nothing unknown is lost | the six conformance cases on memory, dir, overlay and SQLite |
| Generated intents match the inventory | `check-app-intents.sh` in impress-apps' gate; render snapshot tests in koine |

## Open

- Provenance-tagged values and plan-then-execute for the built-in model
  (TR-D11 follow-up).
- Device-scoped consent rules and syncing them.
- Lenses for renames (TR-D12 keeps them possible).
- Enforcing kind validation on write, per kind, after warn mode.
- Verifying `untrusted` and `egress` dynamically (a network and provider
  shim for the effects spy).
- `IndexedEntity` and Spotlight donation from `search_text`.

## Status

Implemented 2026-10-18: TR1–TR10 in koine, TR11 on impress-apps branch
`claude/trust-adopt` (yipihey/impress-apps PR #198, green; merging it is the
person's call).

| WP | State | Where |
|---|---|---|
| TR1 | done | `once` keyed by caller, verb and key, argument fingerprint, in-flight refusal, failures kept, 24 h TTL; `koine_store::once` (`koine/once@1.0.0`, the key's sha256 only) installed by koine-host; verb tasks key by task id |
| TR2 | done | nested calls inherit `tentatively` (a read inside runs); `VerbHandle::fingerprint` on call records and `tools/list` |
| TR3 | done | every backend refuses a dry-run write (`ambient::check_dry_run_write`); `koine-store-overlay` (conformance-tested); rehearsal answers `"tentative":"rehearsed"` with `changes`; 137 of 142 Tier A examples rehearse and change nothing |
| TR4 | done | `koine/pending-call@1.0.0` + `review-request`; `review-service_list/_show/_approve/_reject`; approval runs once as the asking caller on its own task; sweep and 7-day expiry; statuses `input_required` / `auth_required` / `rejected` |
| TR5 | done | `Consent` on descriptors (impact, review hook, targets, alternatives, risk); `ConsentPolicy` (`IMPRESS_VERB_POLICY=consent`); approve `only`/`instead`/`remember`; `koine/consent-rule@1.0.0` bound to fingerprints; `review-service_rules/_revoke-rule` |
| TR6 | done | the kit's Review surface; generated forms preview destructive and external verbs instead of an "I understand" toggle. imbib's and imprint's review lists show the same review requests; their extra fields (targets, reason) are TR11's |
| TR7 | done | `Call::session` on every transport (MCP, `/api/verb`, model conversations, tasks, approvals); `risk(private, untrusted)`; egress from network, provider, device or subprocess reach (`Reach::is_egress`), which must be `external`; the Rule of Two in `ConsentPolicy` |
| TR8 | done | `impress_item::kind`: `KindSchema`, Lexicon-rule `compatible`, registry, version stamps on items and operations, warn-mode validation (`notebook-service_health`), `core/kind-schema@1.0.0` loaded at host start |
| TR9 | done | `Item::extra`, `SurfaceSpec::extra`, `Layout::extra`; six conformance cases on every backend |
| TR10 | done | `intent(…)`, `entity(lookup|search)`, `koine-app-intents`, `<host> app-intents`; output type-checks against the macOS 26 SDK |
| TR11 | done (PR #198) | impress-apps pins koine c5aa252: durable `once`, the review queue and sweep, SQLite's dry-run gate, kind stamps and `extra` column, sync-envelope extras, impress schemas in the kind registry, reach/class agreement, imbib's App Intents (`gen-app-intents`, `scripts/check-app-intents.sh`) and the paper on screen named to Siri; imbib builds on macOS and iOS |

### Left open

- Closed since: nested surface nodes already kept unknown keys and kinds;
  layout panes and windows now keep unknown keys, and an unknown tile kind
  round-trips as a placeholder pane (`UNKNOWN_TILE`). A kind schema with
  `enforce: true` makes stores refuse an invalid insert (conformance
  `an_enforced_kind_refuses_an_invalid_insert`). Generated intents offer
  alternatives through `requestChoice` and make string enumerations
  `AppEnum`s; imprint and implore have generated intents, and the review
  lists show a held call's targets and reason (impress-apps PR #199).
- MCP tasks and URL-mode elicitation for secrets: done (TR-D5).
