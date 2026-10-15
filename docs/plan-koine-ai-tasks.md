# Plan: AI and tasks in koine

**Builds on:** plan-koine-research-os.md (the desk, packages, the kit),
plan-koine-notebook.md (the call log every step lands in), impress-apps
`docs/plan-koine-separation.md` (K1: a runtime is written against the store
port, and a backend implements the port).

## Why

koine is the set of capabilities that apps and whole systems are built from,
for any front end. Two of them still live in impress-apps: the AI kernel
(`impress-ai`, its verbs `impress-ai-service`) and the task kernel (the
`task_*` half of `impel-core`, run by `impel-taskd`). Neither knows what a
publication or a manuscript is. They stayed behind for one reason each:
`AiStore` holds an `Arc<SqliteItemStore>`, and the task store's one
implementation is SQLite's. Both only call methods the store port already
has.

With packages (P-D1..P-D8) the pieces that differ between a small custom app
and a research operating system are data a package carries. The kernels that
run that data belong in koine.

## What moves, what stays

| Today (impress-apps) | Lines | Goes to | Stays |
|---|---|---|---|
| `impress-core` `task.rs` (the state machine) | 315 | `koine-tasks::state` | a re-export |
| `impel-core` `task_store`, `task_executor`, `task_scheduler`, `task_spawn` | 1 221 | `koine-tasks` | re-exports; the SQLite port impl moves to `impress-core` |
| `impress-core` `job.rs` (jobs, the task-event ring) | 641 | `koine-tasks::job`, over the port | a re-export |
| `impress-ai` | 13 270 | `koine-ai`, all but `migration.rs` | the LocalModels import, `AiStore::open` over SQLite |
| `impress-ai-service` | 1 102 | `koine-ai::service` | a re-export |
| `impel-taskd` `desk.rs` | 380 | `koine-ai::desk` | — |
| `impel-taskd` scheduler loop | — | `koine-host` `Config::tasks` | the impress binary: its executors and spawn rules |

Staying in impress-apps: `SqliteItemStore` and the schema registry
(`impress-core`), every domain executor (enrichment, throughline, keyword
tagging, embedding, memory consolidation), `impress-ai-tools` (its grouping
by app and its app probes), `impress-ai-http` (the phone-pairing daemon),
the threads/personas/escalation half of `impel-core`, and the Swift.

## Decisions

### Q-D1. The task kernel is `koine-tasks`

`koine-tasks` holds the state machine (`TaskState`, `transition_op`), the
store port (`TaskStoreApi`, `AgentRunRecord`, `ReviewRequest`), the executor
trait, the scheduler and spawn rules, with their behaviour and their tests
unchanged. The feature `runtime` (default) adds what needs tokio and
async-trait: executors, scheduler, spawn. A store crate takes
`default-features = false` and gets the state machine and the port, which
are synchronous.

The schema refs are the ones every writer uses today (`task@1.0.0`,
`agent-run@1.0.0`, `review-request@1.0.0`, `task-event@1.0.0`); no row
changes spelling.

### Q-D2. Any store is a task queue

`ItemTaskStore<S>` implements the port over any `AttributedStore`. Its
ready queue is a query: `task@1.0.0` rows that are pending or queued and
have a `task_kind`. It keeps the rows whose `next_attempt_at` has passed
and whose `DependsOn` targets are all done, oldest first. That is the
same rule as `SqliteItemStore::ready_tasks`. A backend with a faster
queue implements the port itself: `impress-core` keeps the SQL query
and implements the port for `SqliteItemStore`. The port lives in koine,
so that impl moves out of `impel-core`. The conformance tests run both
implementations through the same assertions.

### Q-D3. A verb is a task

The executor for `task_kind: "koine.verb"` runs `payload.verb` with
`payload.args` through the pipeline, as caller `agent:<scheduler actor>`.
The verb's answer becomes the task's `result`; a refusal fails the task
with its code and message. Any linked verb can be scheduled, retried,
ordered by `DependsOn` and reviewed without a line of executor code. The
pipeline decides who may call, so a policy that denies the verb refuses
the task. A domain does not write a new executor for work a verb already
does.

`task-service_enqueue {verb, args, depends_on?, not_before?}` files one,
and the other task verbs are `list {state?, kind?}`, `show {id}`,
`cancel {id}` and `retry {id}`. Agents schedule work the same way people do.

### Q-D4. Jobs are tasks on any store

The job helpers (ADR-0034 D6: a long-running verb's `task@1.0.0` handle,
its `task-event@1.0.0` ring, cancel by flag, the result written with the
terminal state in one batch) take the store port and move to
`koine-tasks::job`. `impress-core::job` keeps its `&SqliteItemStore`
signatures as one-line wrappers, so its callers and its tests are
unchanged and the SQLite backend stays covered.

The port gains `apply_operation_batch` (several operations, one batch,
all or none) with a default: one target is a clock-guarded batch, retried
when another writer moved the row; several targets apply in order. SQLite
overrides it with its transaction. Jobs and task acquisition were the
two callers of SQLite's own batch.

### Q-D5. A host runs tasks

`koine-host` `Config::tasks` (`--tasks`) runs the scheduler in-process over
the process store: every `poll` (default 2 s), after
`start_delay` (default 0 for a host; an app sets its 90 s). It registers the
verb executor, and with `Config::ai` the AI executors and the desk agent.
A small app turns it on with one flag. A system runs the same host as a
service. `impel-taskd` remains impress's daemon: the same scheduler with its
domain executors registered.

### AI-D1. The AI kernel is `koine-ai`

`koine-ai` is `impress-ai` without the LocalModels importer. It keeps:

- every provider (oMLX, Ollama, OpenAI-compatible servers, Anthropic, …)
  behind `InferenceProvider`, declared once in `CATALOGUE`;
- the registry and `<workspace>/ai/preferences.json`;
- the environment overrides (`IMPRESS_AI_PROVIDER`, `IMPRESS_<P>_URL`, …)
  and the credential chain (environment, then the keychain);
- the schema refs (`conversation@1.0.0`, `chat-message`,
  `tool-invocation@1.0.0`, `agent-run@1.0.0`, `task@1.0.0`);
- the task kinds (`impress.ai.respond`, `impress.ai.suggest-title`).

The verbs keep their names (`impress-ai-service_*`): agents and docs know
them, and K0 makes renames part of a repository cut, not a precondition.
`impress-ai` re-exports `koine-ai`, so no caller in impress-apps changes
its paths.

### AI-D2. The conversation store is over the port

`AiStore` holds an `Arc<dyn AttributedStore>`. `AiStore::from_store`
takes any concrete store and remembers it, so `backing::<S>()` hands
back the typed handle for a caller that needs it (impel-taskd needs
the `SqliteItemStore` for its SQL-only rules). `shared_store()` answers
the port. impress-ai adds `AiStoreSqlite::open(path, actor, kind)`,
which opens SQLite.

### AI-D3. The process has a workspace

`koine_store::process::set_workspace(dir)` / `workspace()` names the
directory the process's state lives in. koine-host sets it to its
workspace. impress-store-service sets it to the directory of the shared
SQLite file. The AI verbs, built with no arguments by the inventory, use
the process store and `<workspace>/ai/`. `koine_ai::service::install`
replaces both, for tests and for hosts that hold their own.

### AI-D4. The model's tools are the linked inventory

`InventoryTools` is a `ToolAdapter` with the policy id `koine-verbs`. It
offers every linked verb as a tool, grouped by service namespace, with the
verb's own input schema. A call goes through the pipeline as the
conversation's agent, so the call log, the notebook and any review policy
see it like any other call. A verb that the reachability gate or the policy
refuses is answered as a refusal, never hidden. impress keeps its own
adapter, `impress-mcp`, which groups tools by app and probes whether each
app is running.

### AI-D5. The desk agent is koine's

The desk bridge (`DeskBridge`, written for impel-taskd in G-D5) moves to
`koine-ai::desk`. It claims each open desk request, runs it as a tagged
conversation with a tool policy the host names, and answers with the reply.
A reply that names `surface:<id>` answers with that view. koine-host runs
it with `InventoryTools` when `Config::ai` is on. impel-taskd runs it with
`impress-mcp`.

### AI-D6. Heavy dependencies are opt-in

The core crates, the grammar and the web bundle never link reqwest or
tokio's network stack. `koine-ai` is linked only by hosts that turn on the
`ai` feature (koine-host turns it on by default). `koine-tasks` without
`runtime` is synchronous and builds for wasm.

### P-D9. Packages carry workflows and AI task categories

A manifest may list `workflows` (`{name, file}`, each file an
`impress/workflow@1.0.0` spec) and `ai_categories` (`{id, name,
description?, parent?, icon?, apps?}`; a leaf's id starts `<parent>.`).

- **Install.** Installing validates every workflow with `impress-workflow`
  before anything is written. Each workflow is stored as a `proposed` row,
  whatever its file says, so a person enables it (the workflow review rule,
  D-R6). Whichever runner the host has runs it: impress's today, see
  Not yet.
- **Upgrade.** The row's id comes from the package and the workflow's name.
  The package's record keeps the digest of what it installed, without
  `state`. An upgrade leaves a row whose content did not change as it is,
  enabled or not. It replaces a row still as shipped and proposes it again.
  It keeps a row the person edited and leaves a deleted one deleted. The
  review says which of these will happen.
- **Categories.** Categories are stored as `koine/ai-category@1.0.0` rows.
  `koine_ai::categories::all_in(store)` is the built-in table followed by
  every installed category whose id the table does not hold, and the
  preferences file assigns models to them by id, as it does today.
- **Remove.** Removing a package deletes its workflows that are still as
  shipped (the rule P-D3 sets for surfaces), keeps the edited ones, and
  removes its categories.

## Order of moves

Each step leaves both repositories green.

1. `koine-tasks`: the state machine, port, scheduler, spawn and jobs over
   the port; `ItemTaskStore`; the verb executor and
   task verbs. Tests: the kernel loop test, run over `MemoryStore` and
   `DirStore`.
2. `koine-ai`: the move, the port-backed `AiStore`, the process workspace,
   `InventoryTools`, the desk bridge, the service with its default
   deployment.
3. `koine-host`: `Config::tasks`, `Config::ai`, the flags, and a test that
   enqueues a verb task over HTTP and reads it done.
4. impress-apps: the koine pin. `impel-core`, `impress-core::task`/`job`,
   `impress-ai` and `impress-ai-service` re-export koine. The SQLite port impl moves into `impress-core`.
   impel-taskd uses `koine_ai::desk`.
5. Packages: `workflows` and `ai_categories` in the manifest; install,
   remove, review.

## Not yet

- The workflow runner (`impress-workflow-service`'s `WorkflowEngine`). Its
  `store`, `message`, `job` and `call` triggers read an arrival feed:
  SQLite's rowid keyset, which also sees rows that sync applied with old
  timestamps. The port has no arrival feed. A query on `created` would
  miss exactly those rows. The runner moves when the port has one, with
  the memory and directory backends implementing it.

- The threads, personas and escalation half of `impel-core` (an agent-team
  model with impress adapters in it).
- `impress-ai-http`, the phone-pairing daemon.
- A `koine-store-sqlite` backend. Until there is one, a standalone host keeps
  its AI history in the directory store.
- Domain executors (enrichment, throughline) rewritten as packaged
  workflows over verbs. Q-D3 and P-D9 are what make that possible.

## What pins this

| Rule | Test |
|---|---|
| The kernel loop (acquire, run, retry with backoff, review, resume) holds on every backend | `koine-tasks` `tests/kernel.rs` over memory and dir stores |
| The generic ready queue matches the SQLite rule (state, kind, backoff, dependencies, order) | `koine-tasks` store tests; `impress-core` port tests |
| A verb task runs the verb as the agent, records the result, fails on a refusal | `koine-tasks` verb executor tests |
| A conversation round-trips over a non-SQLite store | `koine-ai` store tests over `MemoryStore` |
| The desk bridge turns a request into a conversation and its reply into the answer | `koine-ai` `desk` test |
| The model sees the linked verbs and a call is audited as the agent | `koine-ai` tools tests |
| A host with `--tasks` runs an enqueued verb task | `koine-host` `tests/host.rs` |
| A package's workflows install proposed and its categories list | `koine-packages` tests |

## Status

Landed on koine's `research-os` branch: Q-D1 through Q-D5, AI-D1 through
AI-D6 and P-D9.

- **`koine-tasks`.** 14 unit tests and the kernel loop's 14 tests over the
  memory and directory stores, 28 runs. The verb-task tests cover running
  as the agent, a refusal failing the task, dependencies and `not_before`,
  cancel and retry.
- **`koine-ai`.** The moved suite passes over the directory store, 101
  tests. Separate tests cover the desk bridge over a memory store and the
  inventory tools.
- **`koine-host`.** `--tasks` and `--ai` (`tests/host.rs`): an enqueued
  verb runs, and the AI verbs keep `<workspace>/ai/preferences.json`.
- **`koine-packages`.** The workflows and categories tests (P-D9).

The store port gained `apply_operation_batch`, with a conformance case
that every backend passes. impress-apps adopts these crates on its
`koine-research-os` branch: `impel-core`, `impress-core::task`/`job`,
`impress-ai` and `impress-ai-service` re-export koine, and `impel-taskd`
runs `koine_ai::desk`.

