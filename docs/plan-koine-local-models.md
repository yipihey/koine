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
| 7 | A response schema on a turn and a desk request | `store.rs`, `desk.rs`, `service.rs` |
| Later | Spotlight as a tool (LM-D9), MLX through Apple (LM-D6), an MCP client adapter, routing presets (LM-D10) | |

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

- The llama.cpp model for CI: Qwen3 1.7B or 4B GGUF is the suggestion.
  `brew install llama.cpp` on this Mac, which doesn't have it yet.

## Status

- **Package 1 done** on branch `local-models`. `agent_loop.rs` holds the loop,
  with `ToolCall`, `ToolOutcome`, `ToolRunner`, `LoopObserver` and
  `AdapterToolRunner`; `AiTaskExecutor` calls it with unchanged behaviour.
  `koine-ai` tests: 108 pass (103 before, plus 5 new loop tests).
