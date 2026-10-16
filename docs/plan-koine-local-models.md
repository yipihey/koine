# Plan: local models and tool calling, on every platform

**Builds on:** plan-koine-ai-tasks.md (AI-D1..AI-D6: `koine-ai`, the
provider port, `InventoryTools`, the desk agent).

## Why

Koine should run an agent turn on a local model on any platform: a prompt
goes to the model, the model calls a linked verb, Koine runs the verb, the
result goes back to the model, and the model gives a structured answer.
macOS has the best local runtime, Apple's Foundation Models: an on-device
model that is cheap enough to route requests and handle simple actions,
and, from macOS 27, any `LanguageModel` (MLX, Private Cloud Compute) behind
the same session. Linux and Windows have llama.cpp's `llama-server` and
its OpenAI-compatible API. Apple should be an unusually capable provider
behind Koine, not the place Koine's design lives.

Most of this already exists. This plan adds what is missing and changes
nothing that works.

| Proposed | Already in `koine-ai` |
|---|---|
| `ModelProvider` | `InferenceProvider` (`provider.rs`): `models`, `health`, `stream`, `complete` |
| `Tool` / registry | `ToolAdapter` (`tools.rs`): `catalog`, `call(name, Value)`, `provider_id` |
| `ToolSchema` | `ToolDefinition {name, description, input_schema}`, JSON Schema |
| `Conversation` | `ChatRequest`, `ModelMessage`, `ModelToolCall`, `Role::Tool` |
| Response schema | `ResponseFormat::JsonSchema` |
| Capability discovery | `catalogue::Capabilities`, `ModelSummary.{tools, json_schema, thinking, vision}` |
| Routing | `AiRegistry::resolve(ResolveTarget {provider, model, category})`, categories → models in `preferences.json` |
| Koine/MCP tools | `InventoryTools`: every linked verb through `pipeline::invoke_handle` as `Call::agent`, the path `/mcp` takes |
| llama.cpp, vLLM, LM Studio | the `openai-compatible` provider, `Dialect::Generic` |
| Larger local MLX models | oMLX, the same client |

## What we found (2026-10-16)

Read from the macOS 27.0 SDK's `FoundationModels.swiftinterface` and
checked with Swift probes against the on-device model on macOS 26.7.1:

- **The milestone already works through Apple's API on macOS 26.** A JSON
  schema decoded into `GenerationSchema`, a `Tool` with
  `Arguments = GeneratedContent` received `{"city":"Paris","unit":"c"}`,
  returned JSON, and `respond(to:schema:)` answered with a value matching
  the schema, in 1.5 s.
- **Apple's session runs the tool loop.** The session calls `Tool.call`
  itself. `LanguageModelExecutor.respond(to:model:streamingInto:)` is
  public, but its channel events' actions can't be read from outside: it
  is for plugging a model into a session, not for running one round at a
  time. Apple cannot return a round of tool calls the way an
  OpenAI-compatible server does.
- **`GenerationSchema` reads a strict JSON Schema dialect.** Every object
  needs `additionalProperties: false`, `required` and `x-order`. Plain JSON
  Schema fails to decode, and a free-form `{"type": "object"}` cannot be
  expressed at all, so `InventoryTools`' `args: object` cannot go to Apple.
- **macOS 27 adds** `LanguageModel` / `LanguageModelCapabilities`
  (`toolCalling`, `guidedGeneration`, `reasoning`, `vision`),
  `LanguageModelSession(model: some LanguageModel, …)`,
  `GenerationOptions.ToolCallingMode` (`allowed`, `required`,
  `disallowed`), `transcriptErrorHandlingPolicy`, `usage`,
  `PrivateCloudComputeLanguageModel`, and `SpotlightSearchTool` in
  `_CoreSpotlight_FoundationModels`.
- **`MLXLanguageModel` is not in the SDK.** It is the `MLXFoundationModels`
  module of the `mlx-swift-lm` package.
- **llama-server needs `--jinja` for tool calls**, and refuses `tools`
  with `response_format: json_schema` ("Either json_schema or grammar can be
  specified, but not both").

## What is tied to one provider today

- **Apple is `Foreign`.** The catalogue entry has `ExecutionHost::Foreign`,
  and `AiRegistry::provider` answers `ForeignExecutor`: the impress GUI was
  to run it. That Swift (`ImpressAI/AppleFoundationModelsProvider.swift`)
  only completes text, without tools or schemas.
- **The tool loop is inside `AiTaskExecutor::execute`**, together with
  `AiStore` recording and task retries, and it assumes the caller runs each
  round.
- **Structured output stops at the wire.** Providers encode
  `ResponseFormat`, but no turn, conversation or desk request sets it.
- **`InventoryTools` groups verbs by namespace** with a free-form
  `args: object`, which Apple can't express and a small model handles badly.
- **Capabilities are a static table.** No provider reports what it can do
  at run time.
- Leftover oMLX names (`OmlxClient`, `Error::Omlx`, `AiTaskExecutor::new(…,
  omlx)`) are contained. The OpenAI-shaped messages (string `arguments`,
  data-URL images) are fine as Koine's own form.

## Decisions

### LM-D1. Who runs the tool loop is a property of the provider

```rust
pub enum ToolLoop { Caller, Provider }

#[async_trait]
pub trait ToolRunner: Send + Sync {
    /// Policy, the call record and the round limit live here.
    async fn run(&self, call: ToolCall) -> ToolOutcome;
}

trait InferenceProvider {
    fn tool_loop(&self) -> ToolLoop { ToolLoop::Caller }
    async fn stream_with_tools(&self, request: ChatRequest, tools: Arc<dyn ToolRunner>)
        -> Result<EventStream> { self.stream(request).await }
    async fn capabilities(&self, model: &str) -> Capabilities;
}
```

The loop moves out of `AiTaskExecutor` into
`koine_ai::agent_loop::run(provider, request, runner, observer)`. With
`Caller` (llama.cpp, oMLX, cloud) Koine runs rounds as it does now. With
`Provider` (Apple) Koine passes the runner in and the provider calls it.
Either way every tool call goes through the same `ToolRunner`: the same
pipeline, the same `tool-invocation@1.0.0` record and the same limit. The
executor becomes a caller of `agent_loop` that records into `AiStore`, and
its tests stay as they are.

Koine does not imitate Apple's loop, and Apple's types don't leave the
Apple crate.

### LM-D2. The canonical tool call and result

- `ToolDefinition` gains optional `output_schema` (verbs have one) and
  `annotations` (read-only, destructive, from the verbs' effects). It then
  matches an MCP `Tool` field for field.
- `ToolCall {id, name, arguments: Value, raw: String}`. `raw` is kept so
  the record holds exactly what the model emitted; `arguments` is always an
  object.
- `ToolOutcome::{Ok {content: Value}, Refused {code, message}, Failed
  {message}}`. A pipeline refusal is `Refused`, never hidden.
- Every model sees one rendering: `{"ok": true, "result": …}` or
  `{"ok": false, "error": {"code", "message"}}`. MCP's `content`,
  `structuredContent` and `isError` map onto the same type.

### LM-D3. Schemas are lowered in Rust, per profile

`koine_ai::schema::lower(schema, Profile) -> Result<Value, Unsupported>`
inlines `$ref`s and prepares a verb's `schemars` schema for one consumer:

- `AppleGuided`: `additionalProperties: false`, `required` and `x-order` on
  every object, a `title` at the root, and only keywords `GenerationSchema`
  reads;
- `LlamaGrammar`: what llama.cpp's schema-to-grammar conversion accepts;
- `OpenAiStrict`: OpenAI strict mode.

A schema that can't be lowered is refused with the path that failed;
nothing is dropped silently. Because lowering is in Rust it is tested on
every platform, and Swift only decodes the result.

### LM-D4. `InventoryTools` gains a flat mode

`InventoryTools::flat(verbs)` offers one tool per allowed verb with the
verb's own input schema, lowered for the provider. Apple needs this; small
models do better with it. The grouped mode stays for large models. The
conversation's tool policy names the allowlist.

### LM-D5. Apple is a native provider in `koine-ai-apple`

`koine-ai-apple` is a new crate, built only on macOS behind a feature. It
holds a Swift package built by `build.rs` (`swift build`, linked
statically) that exposes a small C interface carrying JSON strings and
callbacks:

- `fm_availability() -> json`
- `fm_respond(request_json, on_event, on_tool_call, ctx)`
- `fm_tool_result(call_handle, result_json)`

About 300–400 lines of Swift: build a `LanguageModelSession` from the
request (system messages become instructions, earlier turns become
`Transcript` entries including tool calls and outputs); make one bridged
`Tool` per definition, whose `call` sends the JSON arguments to Rust and
waits for `fm_tool_result`; call `respond` / `streamResponse(to:schema:)`
when a response schema is set; map guardrail, refusal, context and
rate-limit errors to Koine's `Error` (one new variant, `Refused`). macOS 26
gives the system model, tools, schemas and streaming. macOS 27, behind
`#available`, adds `session(model: some LanguageModel)`, `ToolCallingMode`,
`transcriptErrorHandlingPolicy` and `usage`.

The provider reports `tool_loop() = Provider` and its capabilities from
the live model. `AiRegistry::register_native(id, provider)` installs it,
so `apple-on-device` stops being `Foreign` in a host that links it; the
impress GUI, which doesn't, keeps the current behaviour. impress-apps may
later import the same Swift package in place of its text-only provider.

UniFFI suits Swift calling Rust (the impress GUI). Here a Rust binary
calls Swift, so the plain C interface is thinner. If linking Swift into
`koine-host` proves painful, the fallback is a small Swift helper process
speaking the same JSON over stdio.

### LM-D6. MLX through Apple is a separate Swift option

`MLXLanguageModel` brings mlx-swift and Metal into the build. oMLX already
gives Koine the same models out of process, through the OpenAI-compatible
client. Through Apple, MLX is a second Swift product added when a single
process or iOS needs it.

### LM-D7. llama.cpp is a catalogue entry

A `llama-cpp` entry (or `Dialect::LlamaCpp`) with default
`http://127.0.0.1:8080`. Its health check reads `/props` (chat template,
context size), and the docs say `--jinja` is required. It reports
`structured_with_tools = false`. When a provider can't combine the two,
`agent_loop` runs tool rounds without the schema and then one more round
with tools removed and the schema set. That is the pattern the executor
already uses at the round limit. Ollama, vLLM, SGLang and remote servers
are then entries or dialects; nothing in tools or agents changes.

### LM-D8. Capabilities: shared, and provider-specific

`Capabilities` becomes a starting point each provider refines at run time
(Apple's live availability and capability list, llama.cpp's `/props`).

- **Shared:** streaming, tool calling, structured output with its schema
  profile, `structured_with_tools`, tool choice (auto, required, none),
  parallel tool calls, image input, reasoning output, context window,
  maximum output, `tool_loop`.
- **Provider-specific**, under `extensions: BTreeMap<String, Value>`, which
  core code never branches on: Apple guardrails, use cases and adapters,
  the Private Cloud Compute quota, prewarm and transcript reuse; llama.cpp
  grammars and slot control; oMLX load and unload; reasoning signatures.
- **Embeddings** are a separate port, not a chat capability.

### LM-D9. OS search is a tool, not a model capability

On macOS the Swift bridge exposes `SpotlightSearchTool` as a `ToolAdapter`
(`os-search`), so any model on the Mac can use it, a llama.cpp one
included. Linux and Windows don't register it. Being present is the
capability.

### LM-D10. Routing stays in the registry

No new router. Categories already map to models; presets name a small
local model (on-device) for routing and simple actions, a larger local
model (oMLX or llama.cpp) for reasoning and code, and a frontier model
when the task needs it or the person asks. All three are providers behind
the same port.

### LM-D11. A desk request carries a category, and the desk routes by it

Today the desk resolves one model per pass (`ResolveTarget::default()`), so
every request goes to the device default and the task categories are unused.

- `agent-service_ask` takes an optional `category` (a task-category id), kept
  on the request row. The `/ask` page offers it as a short choice (default,
  or "use the strongest model", which is `agent.frontier`).
- The desk resolves each request on its own: `ResolveTarget { category }`
  with the request's category, so preferences decide which model a category
  gets, as they already do for the other categories.
- A request with no category is triaged when the category `agent.route` is
  assigned: that model (the on-device one, typically) answers one structured
  call whose schema is an enum of the routable category ids, and the request
  is resolved with the answer. With no `agent.route` assignment, or if triage
  fails, the request uses the default, as today. The triage is recorded on
  the request.
- Two new built-in categories: `agent.route` (triage) and `agent.frontier`
  (explicitly asked for the strongest model). A preset verb,
  `impress-ai-service_apply-routing-preset {preset: "local-first"}`, writes the
  three-tier assignment from what is ready on the device: `agent.route` →
  apple-on-device; `agent` and its children → the largest ready local model
  (oMLX or llama.cpp); `agent.frontier` → the first ready cloud provider. It
  writes preferences only; a person can change any of them.
- The executor's tool catalog follows the resolved provider: a provider whose
  tool loop is Apple's gets the flat, Apple-lowered `InventoryTools`
  (package 6 note), every other provider the grouped one.

## Work packages

Each one is reviewable on its own and leaves the workspace green.

| # | Package | Files |
|---|---|---|
| 1 | `agent_loop`, `ToolRunner`, `ToolOutcome` (LM-D1, LM-D2); executor behaviour unchanged | `koine-ai/src/agent_loop.rs`, `executor.rs`, `tools.rs` |
| 2 | Provider-run loop, `capabilities()`, `structured_with_tools`, the final schema round; a tool call with empty arguments runs with `{}` instead of failing; the conformance suite over two scripted providers | `provider.rs`, `catalogue.rs`, `tests/conformance.rs` |
| 3 | Schema lowering profiles with snapshots (LM-D3) | `koine-ai/src/schema.rs` |
| 4 | Flat `InventoryTools`; `output_schema` and `annotations` (LM-D4) | `inventory_tools.rs`, `types.rs` |
| 5 | llama.cpp entry, `/props`, the tools-plus-schema rule; the llama.cpp milestone (LM-D7) | `catalogue.rs`, `providers/openai_compat.rs` |
| 6 | `koine-ai-apple`, `register_native`; the Apple milestone on macOS 26, macOS 27 features behind `#available` (LM-D5) | new crate, `registry.rs`, `koine-host/src/tasks.rs` |
| 7 | A response schema on a turn and a desk request; per-request routing and triage (LM-D11); the executor's catalog follows the provider | `store.rs`, `desk.rs`, `service.rs`, `categories.rs`, `executor.rs`, `koine-agent`, `koine-web/src/ask_page.rs`, `koine-host/src/tasks.rs` |
| Later | Spotlight as a tool (LM-D9), MLX through Apple (LM-D6), an MCP client adapter | |

The milestone is reached at packages 5 and 6, with the same `ChatRequest`,
`ToolDefinition`, `ToolRunner` and test fixture on both backends.

## What pins this

| Rule | Test |
|---|---|
| A tool call on any backend runs the right verb with the right arguments, feeds the result back, and ends in an answer that matches the schema | `koine-ai` `tests/conformance.rs`, one fixture (`weather_lookup`, and a `deny_me` the policy refuses) over: a scripted provider in each loop mode (CI, every OS); llama-server with a small tool-capable GGUF (`KOINE_TEST_LLAMACPP_URL`, a Linux CI job); Foundation Models (`KOINE_TEST_APPLE_FM=1`, a Mac with Apple Intelligence) |
| A refusal reaches the model as `ok: false`; the round limit holds; every call is recorded | the same suite |
| Lowering is deterministic, and every Apple-profile schema decodes | `schema` snapshot tests over the linked verbs; a Swift test that decodes each into `GenerationSchema` |
| llama-server's tools-plus-schema refusal is handled | `openai_compat` mockito tests replaying recorded traffic |
| A desk request is answered by a local model that calls a linked verb | `koine-host` `tests/host.rs` with `--ai` |

Tests assert properties, not model wording.

## Open

- A llama.cpp CI job: the model is chosen (Qwen3-4B-Instruct-2507 Q4_K_M
  works, see package 5); the job itself, with a cached GGUF, is not added.

## Status

- **Package 1 done** on branch `local-models`. `agent_loop.rs` holds the loop,
  with `ToolCall`, `ToolOutcome`, `ToolRunner`, `LoopObserver` and
  `AdapterToolRunner`; `AiTaskExecutor` calls it with unchanged behaviour.
  `koine-ai` tests: 108 pass (103 before, plus 5 new loop tests).
- **Package 2 done** on branch `local-models`. `InferenceProvider` gains
  `tool_loop()`, `capabilities(model)` and `run_turn()`; `agent_loop::run`
  hands a `ToolLoop::Provider` turn to the provider behind a limit-enforcing
  runner and, for a Caller loop whose provider has
  `structured_with_tools = false`, holds the schema back for a final
  tools-free round (user-role instruction). A tool call with empty arguments
  now runs with `{}`. `conformance.rs` holds the shared fixture;
  `tests/conformance.rs` runs it over scripted providers in both loop modes.
  `koine-ai` tests with `--features executor`: 110 lib (108 before, plus 2)
  and 6 conformance. Live (`KOINE_TEST_OPENAI_COMPAT_URL`, oMLX): Qwen3.5-4B-4bit
  and Qwen3-Coder-30B-A3B-Instruct-8bit both pass.
- **Package 3 done** on branch `local-models`. `koine-ai/src/schema.rs`
  (`koine_ai::schema::{lower, Profile, Unsupported}`) lowers a `schemars`
  schema for `AppleGuided`, `LlamaGrammar` or `OpenAiStrict`; what a profile
  cannot say is refused with the path. With `--all-features` 33 verbs are
  linked: `LlamaGrammar` lowers all 33; `AppleGuided` and `OpenAiStrict`
  lower 32 and refuse `task-service_enqueue` at `args` (any value: it takes
  the arguments of any verb), which is right. With `KOINE_TEST_APPLE_FM=1`
  on macOS 27.0.1, `swiftc` decodes all 39 Apple-profile outputs (fixtures,
  derived structs, verbs) into a `GenerationSchema`. Foundation Models
  needed one rule beyond the plan: an `anyOf` node carries a `title`.
  Vendor `x-*` keywords (`x-private`) are dropped for Apple and OpenAI.
  `x-order` is alphabetical: the workspace's `serde_json` has no
  `preserve_order`, so source field order is lost; it matters only where
  generation order does (a response schema), not for tool arguments.
  `koine-ai` tests: 123 lib with `--features executor` (110 before, plus 13),
  6 conformance, 1 inventory_tools, 4 schema_lowering.
- **Package 4 done** on branch `local-models`. `ToolDefinition` gains optional
  `output_schema` and `annotations` (serde-defaulted and skipped when absent,
  so stored definitions still load) plus `ToolDefinition::new`; the providers
  build their own wire JSON and never send them. `InventoryTools::flat(agent,
  verbs, profile)` offers one tool per allowed verb under `koine-verbs`, named
  by the verb, with its input schema (lowered for the profile, raw when
  `None`), output schema and MCP annotations. An allowlist entry ending in
  `_` is a namespace prefix. Unlinked, unavailable, deprecated and excluded
  verbs are absent; verbs the profile cannot express are skipped and listed by
  `skipped()`. A call's arguments are the verb's own, through
  `pipeline::invoke_handle`; a name outside the catalog is `<name>: not
  offered`. `koine-ai` tests with `--features executor`: 126 lib (123 before,
  plus 3), 6 conformance, 5 inventory_tools (1 before, plus 4), 4
  schema_lowering.
  - **Downstream:** impress-apps builds `ToolDefinition` with struct literals in
    `crates/impress-ai-tools/src/lib.rs` and
    `crates/impress-store-ffi/src/ai_registry.rs`; on the next koine pin those
    need `..Default::default()` or `ToolDefinition::new`.
- **Package 6 done** on branch `local-models`. New crate `koine-ai-apple`: a
  SwiftPM static library `KoineFM` (`swift/`, 275 lines) behind a C ABI of
  JSON strings and callbacks (`koine_fm_availability`, `_respond`,
  `_tool_result`, `_cancel`, `_free`), built by `build.rs`; `wire.rs` builds the
  request JSON and parses events on every OS; `macos.rs` is the FFI.
  `AppleFoundationModels` reports `ToolLoop::Provider` and live capabilities
  (`structured_with_tools = true`). `AiRegistry::register_native(id, provider)`
  makes `provider(id)` answer it before the `Foreign` check; the Apple catalogue
  entry and model now say tools + json_schema + `structured_with_tools`. A
  guardrail or refusal is `LoopError::Exhausted("apple-on-device refused: ...")`
  (no new `Error` variant). A turn dropped mid-flight cancels the Swift task and
  leaks its few-byte callback context on purpose. Live on macOS 27.0.1
  (`KOINE_TEST_APPLE_FM=1`, 6 tests, 6.4-7.2 s in total, repeated 4 times, all
  pass): the weather milestone (one `weather_lookup` call with `{"unit":"c",
  "city":"Paris"}`, answer `temperature: 18` in the schema, 1.4 s), the
  refused `deny_me` (the model reports the refusal, 1.0 s), the round limit (the
  call gets `tool_limit` and the model still answers), a plain `stream()`
  title, and three turns cut at 50 ms followed by a full turn. `koine-ai` tests
  with `--features executor`: 127 lib (126 before, plus 1), 6 conformance,
  5 inventory_tools, 4 schema_lowering; `koine-ai-apple`: 8 unit tests.
  - **Run it in the host:** `cargo run -p koine-host --features apple-fm -- ...`
    (off by default: it adds a Swift build, and the binary links
    FoundationModels, so it needs macOS 26 or later). The host logs whether the
    on-device model is available.
  - **Package 7 gets the desk's tools for Apple.** The desk uses the grouped
    `InventoryTools`, whose namespace tools carry a free-form `args` object that
    Apple cannot express, so with Apple selected the desk runs without tools.
    The executor should pick a flat, Apple-lowered catalog
    (`InventoryTools::flat(.., Profile::AppleGuided)`) when the resolved
    provider is Apple.
  - **Review fix:** a turn Rust cancels never frees its callback context,
    even after its terminal event. Cancelling resumes a tool call Swift may
    have parked but not yet announced, which would then reach a freed
    context. The context of a turn that ends on its own is freed.
- **Package 5 done** on branch `local-models`. New catalogue entry `llama-cpp`
  ("llama.cpp (llama-server)", Local, `Dialect::LlamaCpp`, default
  `http://127.0.0.1:8080`, optional bearer, `structured_with_tools = false`,
  vision false), placed after Ollama and before the generic entry. The dialect
  uses the standard `/v1` paths, lowers every tool `input_schema` and a
  JSON-schema `response_format` with `Profile::LlamaGrammar` (raw schema if
  lowering refuses), and reads `GET /props` for health: `n_ctx` (from
  `default_generation_settings`, also set as `max_context_window` on listed
  models), `model_alias`/`model_path`, and `chat_template`; with no template,
  `jinja: false` or `chat_template_caps.supports_tools == false` the detail says
  tool calls need `--jinja`. The `/props` of build b11669 has no `jinja` field
  (that flag is not visible there), so a server started without `--jinja`
  cannot be told apart from its health alone; it fails at the first tool call.
  The round-limit instruction is now a `Role::User` message for every provider
  (tests updated in `agent_loop.rs` and `executor.rs`). `koine-ai` tests with
  `--features executor`: 131 lib (127 before, plus 4), 7 conformance (6 before, plus
  `live_llama_cpp`, which skips unconfigured), 5 inventory_tools, 4 schema_lowering; the catalogue
  and `service.rs` provider-count tests now expect 9 entries.
  - **Live** (`KOINE_TEST_LLAMACPP_URL`, `live_llama_cpp`, Qwen3-4B-Instruct-2507
    Q4_K_M on this Mac): the weather turn (one `weather_lookup`, answer in the
    schema) and the refused `deny_me` pass, three runs of 18.1 s, 15.4 s and
    18.0 s in total (both turns). oMLX live (Qwen3.5-4B-4bit) still passes
    (3.0 s).
  - **The command that works:**
    `llama-server -hf unsloth/Qwen3-4B-Instruct-2507-GGUF:Q4_K_M --jinja -c 16384 --host 127.0.0.1 --port 8080`
    (`--api-key KEY` if the entry gets a bearer).
  - **Linux and Windows:** the same command and the same `llama-cpp` entry apply;
    `llama-server` is a release binary or a package on both, and nothing in
    the dialect is OS-specific. A CI job would need `llama-server` on the
    runner, the GGUF restored from a cache keyed on the model file (about
    2.5 GB), the server started in the background and polled on `/health`,
    then `KOINE_TEST_LLAMACPP_URL` set for `cargo test -p koine-ai --test
    conformance live_llama_cpp`. CPU-only is slow but this fixture is a few
    short turns. Not added here.
- **Package 7a done** (LM-D11; the response schema on a turn is still to come).
  `agent-service_ask` takes `category` (shape-checked in `koine-agent`, which
  cannot depend on `koine-ai`; the desk checks it against the table and the
  installed categories); `RequestRow` gains `category`, `routed_category` and
  `triage_model`, all skipped when absent. New built-in categories
  `agent.route` and `agent.frontier` (no app lists them). `DeskBridge` gains
  `with_registry(ai, registry)`: each request is resolved on its own
  (`ResolveTarget { category }`: assignment, then selection, then first
  ready); `DeskBridge::new(ai, provider, model)` is unchanged and stays fixed.
  An unknown category declines the request with a message that says so; a
  retryable failure still releases. With no category and an enabled
  `agent.route` assignment, one structured call on that model (an enum of the
  categories with an enabled primary assignment, minus `agent.route`, plus
  `agent`; 30 s limit) picks the category; any failure or an answer outside
  the enum falls back to the default and is logged. The choice is written to
  the row. `impress-ai-service_apply-routing-preset {preset: "local-first"}`
  assigns `agent.route` to apple-on-device, `agent` to oMLX (else another ready
  local provider) and `agent.frontier` to the first ready cloud provider,
  skipping with a reason what is not ready; it writes preferences only.
  `AiTaskExecutor::with_native_tool_adapter` supplies the definitions and the
  runner for a task whose resolved provider has `ToolLoop::Provider`;
  koine-host (feature `apple-fm`) passes a flat, Apple-lowered
  `InventoryTools` over `DEFAULT_NATIVE_TOOLS` (8 read-only verbs, 2.7k
  characters of name, description and input schema; the cap is 6000, applied
  in allowlist order), overridable with `--native-tools a,b,c`. `/ask` has a
  "Use the strongest model" checkbox (`category: "agent.frontier"`) and shows
  the category or the routing on a listed request.
  - **Beyond the plan:** `agent` is a category group, which
    `set_task_category` refused; it now accepts `agent` (the desk resolves by
    it). `AiRegistry::default_model` is public. `koine-ai` depends on
    `tracing`. `categories::{triage_request, parse_triage}` hold the triage
    call so the Apple live test uses the production request.
  - **Registry quirk:** `resolve` takes the model from the device selection
    before the category's own model when both name the same provider, so two
    categories on one provider with different models only work while the
    selection is on another provider.
  - **Live (macOS 27.0.1, `KOINE_TEST_APPLE_FM=1`, `koine-ai-apple` test
    `triage`):** enum of agent, agent.frontier, research.summarize,
    writing.rewrite. "What is the capital of France?" gave `agent`;
    "Summarise the abstract of this paper in two sentences." gave
    `research.summarize`; "Review this proof for errors: ..." gave `agent` (not
    `agent.frontier`), 0.4-0.9 s each.
  - **Downstream:** impress-apps keeps `DeskBridge::new` working; `impel-taskd`
    may move to `with_registry`. `TaskRuntime` and `Config` gain `native_tools`.
