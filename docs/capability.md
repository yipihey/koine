# How to add a Rust capability

This is the guide for an agent that has koine and a Rust library and nothing
else. At the end the library's functions are verbs: each one an MCP tool, a
CLI subcommand, an agent tool and a generated form, from one declaration.

`scripts/new-app.sh <name> --lib <crate>` writes the crate this guide
describes, with every step below already in place for one verb (`echo`).
Read this once, then work from its output. The checklist at the end is the
definition of done.

Every attribute and message in this file is the real one: the macros are in
`crates/impress-service-macros/src/lib.rs`, and
`crates/impress-service-macros/tests/ui/*.stderr` holds the exact text of
each refusal quoted below. The vocabulary of adverbs and adjectives is
[grammar.md](grammar.md); the plan this guide implements is
[plan-koine-host.md](plan-koine-host.md) (H-D6).

## 1. What a capability is

A capability is one `#[impress_service]` trait. Each method marked
`#[impress_method]` is a verb. `impress_service_impl!` binds the trait to an
implementation and, for every verb, emits:

- one static `VerbDescriptor` (`impress-service-core`): the name, the
  description, the input and output JSON schemas, the safety class, the
  effects, the adverbs it supports, its examples;
- a projection of it into the process-wide `inventory` as an
  `McpToolDescriptor` and a `CliSubcommand`.

Everything else is a reader of that inventory: an MCP host lists the
descriptors as tools, the CLI builds a subcommand from each, the generated
form (`impress-verb-surface`) builds a surface from each schema, impel
offers each as an agent tool, and the HTTP host runs one by name
(`POST /api/verb/<name>`; `koine-host`, plan-koine-host H-D4). A hosted
capability is therefore never GUI code and never a hand-written server: if
you are writing either, stop and give the library a verb.

The name of a verb is `kebab(<trait name>)_kebab(<method name>)`. A trait
called `StatsService` with a method `trimmed_mean` is the tool
`stats-service_trimmed-mean`. Name the trait `<Thing>Service` and the prefix is
always `<thing>-service`.

A verb runs through the invoker pipeline (`impress-service-core::pipeline`):
identity, strict arguments, reachability, policy, span, the handler, envelope,
audit. You write the handler; the other seven layers are not yours to repeat.

## 2. The service crate

```
crates/<name>-service/
├── Cargo.toml        the library, impress-service-core, impress-service-macros,
│                     serde, serde_json, schemars
└── src/
    ├── lib.rs        mod dto; mod service; #[cfg(test)] mod selftest;
    │                 and `pub fn ensure_linked() {}`
    ├── dto.rs        the types that cross the wire (section 4)
    ├── service.rs    the trait, its implementation, `impress_service_impl!`
    └── selftest.rs   Tier A (section 8)
```

Two dependencies are easy to forget. The macros expand to `::serde::Deserialize`
and to `JsonSchema` derives, so the crate that calls them depends on `serde`
and `schemars` itself, not only through `impress-service-core`.

Verbs register themselves in the inventory only in a binary that links the
crate. A host `main` must refer to something in the service crate for the
linker to keep it; `ensure_linked()` is that something. A verb missing from
the list of a running host is almost always this.

## 3. A verb, in full

Library functions (in a crate named `mini-stats`):

```rust
pub fn mean(values: &[f64]) -> Option<f64>;
pub fn median(values: &[f64]) -> Option<f64>;
```

Their verb, `service.rs`:

```rust
use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::dto::Estimate;

#[impress_service]
pub trait StatsService: Send + Sync + 'static {
    /// The arithmetic mean of a list of numbers. Refuses an empty list.
    #[impress_method]
    #[impress_example(
        name = "three",
        args = r#"{"values": [1, 2, 6]}"#,
        expect = r#"{"ok": true, "value": 3.0}"#
    )]
    async fn mean(&self, values: Vec<f64>) -> Estimate;
}

#[derive(Clone, Default)]
pub struct DefaultStatsService;

impl DefaultStatsService {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl StatsService for DefaultStatsService {
    async fn mean(&self, values: Vec<f64>) -> Estimate {
        match mini_stats::mean(&values) {
            Some(value) => Estimate::value(value),
            None => Estimate::refused("empty-input", "mean of no numbers is undefined"),
        }
    }
}

impress_service_impl! {
    service = StatsService,
    impl = DefaultStatsService,
    instance = DefaultStatsService::new,
    safety = read_only,
    since = "0.1.0",
    effects = { reads: [], writes: [], reach: [] },
    methods = [
        /// The arithmetic mean of a list of numbers.
        mean(
            /// The numbers to average. At least one.
            values: Vec<f64>,
        ) -> Estimate,
    ],
}
```

What each part is for:

- **The `///` on the trait method is the description.** It is what an agent
  reads to decide whether to call the verb. A method without one is a compile
  error naming it. Say what the verb does, what it refuses, and what the
  result means; do not repeat the name.
- **Arguments are the method's parameters**, and become the keys of one JSON
  object. Rust allows no doc comment on a trait method's parameter, so an
  argument is described by a `///` on it inside `methods = [...]`; that text
  becomes its schema `description` (MCP `inputSchema`, CLI `--help`). Mark an
  argument the call log must not keep (a password, a token) with
  `#[impress_private]` in the same place; its schema property gets
  `"x-private": true` and the log stores a length and a hash.
- **`methods = [...]` repeats each signature.** It is the one thing the macro
  cannot read off the trait. The names, argument types and return type must
  match the trait's; a mismatch is a type error in generated code.
- **`instance = ...`** is an expression that yields the implementation each
  call. A function path (`DefaultStatsService::new`) or a closure
  (`|| DefaultStatsService`); a service holding a handle returns a clone of
  a `static`.
- **`#[impress_example(name, args, expect)]`** is a named call. `args` is the
  argument object as JSON text, `expect` an optional subset the result must
  contain; both are checked as JSON at compile time. Examples are what Tier A
  runs (section 8), what the generated form prefills from, and what an agent
  reads to learn the shape. Give every verb at least one.
- **`since`** is the version the verbs appeared in.

A verb never returns `Result`. The generated invoker serialises whatever the
method returns, so a `Result` would arrive as `{"Ok": ...}` and `{"Err": ...}`.
A verb that can refuse says so in its result; see "Refusing" below.

### Refusing

A result whose top-level `ok` is `false` is a refusal: the CLI exits 3, HTTP
maps its `code` to a status, MCP carries it as a result rather than a
transport error. A result with no `ok` field is a success. Give a result that
can refuse `ok`, and, when it refuses, `code` and `message`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Estimate {
    /// False when the call was refused; then `code` and `message` say why.
    pub ok: bool,
    /// The estimate, when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// A stable kebab-case reason (`empty-input`), when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The reason in a sentence, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Estimate {
    pub fn value(value: f64) -> Self {
        Self { ok: true, value: Some(value), code: None, message: None }
    }
    pub fn refused(code: &str, message: &str) -> Self {
        Self { ok: false, value: None, code: Some(code.into()), message: Some(message.into()) }
    }
}
```

Use the generic codes where one fits (`impress_service_core::refusal::codes`:
`invalid-argument`, `not-found`, `conflict`, `store-error`, ...) and a
kebab-case code of your own where none does. A bad argument the schema cannot
express (an empty list, a negative count) is `invalid-argument`.

## 4. DTO rules

A DTO is a type that crosses the wire: an argument or a result. The input
schema of a verb is the derived schema of its parameters; the output schema is
the derived schema of its return type. Both come from `schemars`, both are
published to every agent, and the strict-argument check walks them. So every
argument type derives `Deserialize` and `JsonSchema`, and every result type
derives `Serialize` and `JsonSchema`; the macro refuses a verb whose types do
not, at the type, with the fix in the message.

### The five shapes that work

1. **A struct of owned fields**, serde and schemars derives, `///` on each
   field (it becomes the schema description):

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
   pub struct Summary {
       /// How many numbers were summarised.
       pub count: u64,
       /// The smallest, absent for an empty list.
       pub min: Option<f64>,
   }
   ```

2. **An enum of unit variants**: a closed vocabulary, shown as a `select` in
   the generated form. Pick the wire spelling once:

   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
   #[serde(rename_all = "kebab-case")]
   pub enum Estimator {
       Mean,
       Median,
       TrimmedMean,
   }
   ```

3. **An internally tagged enum of structs**: a choice that carries data. The
   tag decides which variant a strict-argument check walks, so a typo inside
   a variant is still caught:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
   #[serde(tag = "kind", rename_all = "kebab-case")]
   pub enum Selection {
       /// Rows `start..end`.
       Range { start: u64, end: u64 },
       /// These rows.
       Indices { indices: Vec<u64> },
   }
   ```

4. **`Option<T>`, `Vec<T>` and `BTreeMap<String, T>`** over any of these. An
   `Option` argument is an optional key; every other argument is required. A
   struct field that should default when absent says `#[serde(default)]`.
   Use `BTreeMap`, not `HashMap`, so the order a result prints in is stable;
   a map's keys are always strings on the wire.

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
   pub struct Histogram {
       pub edges: Vec<f64>,
       pub counts: Vec<u64>,
       /// Free-form annotations by name.
       pub labels: BTreeMap<String, String>,
       pub note: Option<String>,
   }
   ```

5. **A newtype with `#[serde(transparent)]`**: a distinct type for an
   identifier or a unit, one JSON value on the wire:

   ```rust
   #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
   #[serde(transparent)]
   pub struct SampleId(pub String);
   ```

Prefer `String`, `bool`, `u64`/`i64`, `f64` and `Vec` of those as arguments:
the generated form gives them a text, toggle or number field. An object,
array or enum argument becomes a raw-JSON text field, and works, but is
harder for a person to fill in. For a result, a flat object shows as a
key-value list and a `Vec` of flat objects as a table; a result with nested
arrays shows as raw JSON. A genuinely free-form payload is `serde_json::Value`,
whose schema says nothing about it; reach for it last.

### The three that do not work, and the wrapper to write

**A reference or a lifetime.** An argument is read from a JSON object into an
owned field, so `&str`, `&[f64]`, `Cow<'_, str>` and a type with a lifetime
parameter cannot be one. The macro refuses it at the argument:

```
error: argument `message` borrows; a verb's arguments are read from a JSON
object into owned fields — write the owned type (`String` for `&str`, `Vec<T>`
for `&[T]`, `PathBuf` for `&Path`) and borrow inside the method
```

Library: `pub fn tokens(text: &str) -> Vec<Token<'_>>`. Wrapper:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenDto {
    pub text: String,
    pub start: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenList {
    pub tokens: Vec<TokenDto>,
}

// In the implementation: borrow the owned argument, copy into the DTO.
async fn tokens(&self, text: String) -> TokenList {
    TokenList {
        tokens: lib::tokens(&text)
            .into_iter()
            .map(|t| TokenDto { text: t.text.to_string(), start: t.start as u64 })
            .collect(),
    }
}
```

**A generic the macro cannot name.** A schema is of a concrete type, so a
generic method on the trait, or an `impl Trait` or `dyn Trait` argument, has
no schema. The macro refuses a generic method:

```
error: #[impress_method] `SizeService::size_of` is generic; a verb's arguments
become a JSON schema, which needs concrete types — write one method with
concrete DTOs per instantiation
```

Library: `pub fn fit<M: Model>(model: &M, data: &[f64]) -> Fit`. Wrapper: one
verb per instantiation you want an agent to have, or one verb taking a tagged
enum (shape 3) that picks the instantiation, dispatching inside:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "model", rename_all = "kebab-case")]
pub enum ModelSpec {
    Linear,
    Polynomial { degree: u32 },
}

async fn fit(&self, model: ModelSpec, data: Vec<f64>) -> FitDto {
    match model {
        ModelSpec::Linear => FitDto::from(&lib::fit(&lib::Linear, &data)),
        ModelSpec::Polynomial { degree } => FitDto::from(&lib::fit(&lib::Poly::new(degree), &data)),
    }
}
```

**A foreign type without `JsonSchema`.** The orphan rule stops you
implementing `JsonSchema` or `Serialize` for the library's type, and the
library will not derive them for you. A verb whose argument or result is one
fails at the type:

```
error[E0277]: argument `Matrix` of a verb needs `serde::Deserialize` and `schemars::JsonSchema`
  |         trace(matrix: Matrix) -> Trace,
  |                       ^^^^^^ this argument type has no `Deserialize` + `JsonSchema`
  = note: derive both on it, or — for a type from another crate — write a DTO that
          derives both and convert with `From`/`TryFrom` inside the method
```

(For a result the same message says `Serialize`.) Wrapper: a DTO of the
fields a caller needs, `From` the library type for a result, `TryFrom` the DTO
for an argument, the conversion refusing what the library type cannot hold:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MatrixDto {
    pub rows: u64,
    pub cols: u64,
    /// Row-major, `rows * cols` long.
    pub data: Vec<f64>,
}

impl From<&lib::Matrix> for MatrixDto {
    fn from(m: &lib::Matrix) -> Self {
        Self { rows: m.rows() as u64, cols: m.cols() as u64, data: m.as_slice().to_vec() }
    }
}

impl TryFrom<MatrixDto> for lib::Matrix {
    type Error = String;
    fn try_from(dto: MatrixDto) -> Result<Self, String> {
        if dto.data.len() as u64 != dto.rows * dto.cols {
            return Err(format!("data has {} numbers, rows * cols is {}", dto.data.len(), dto.rows * dto.cols));
        }
        Ok(lib::Matrix::from_row_major(dto.rows as usize, dto.cols as usize, dto.data))
    }
}
```

Call `try_from` in the method and answer `invalid-argument` on `Err` (the
refusal pattern of section 3).

## 5. Effects

`effects` says what a verb touches. It is required once per service in
`impress_service_impl!`; a service without it does not compile:

```
error: missing `effects = { reads: […], writes: […], reach: […] }`: declare what the
service's verbs read, write and reach, e.g. `effects = { reads:
["bibliography-entry"], writes: [], reach: [fs] }`, or `effects = {}` for a service
that touches nothing; a verb that differs from the service says so with
`#[impress_method(effects(reads = […], writes = […], reach = […]))]` on the trait
```

- `reads` and `writes` are lists of **kinds**: a canonical ref string from
  `schema-refs.json` (checked when the macro expands; a misspelt ref is a
  compile error, because the store matches refs by exact equality and a wrong
  one reads zero rows forever); `target(arg)` or `children(arg)`, the kind of
  the record, or the records under the record, that the argument `arg` names;
  `prefix("app/ui/")`, every kind under a prefix; or `any("reason")`.
- `reach` is where the verb leaves the process: `fs`, `network`,
  `subprocess`, `device`, `provider` (an AI provider) or `app("<id>")` (another
  running app).
- A method that differs from the service default replaces the **whole** set:
  `#[impress_method(effects(reads = ["demo/sample"], writes = [], reach = [fs]))]`.
  `effects()` is a method that touches nothing under a service that does.

Be honest, and be exact. Pure computation over the arguments, which is most
of a wrapped library, is `{ reads: [], writes: [], reach: [] }`: write the
empty lists, they are a claim. A verb that reads a file declares `reach: [fs]`;
one that calls a web API, `network`; one that shells out, `subprocess`. Name the
kind the verb reads or writes, not a broader one. `any("reason")` is honest
only when the kind genuinely depends on data you cannot name in advance (a
verb that opens whatever record an id names, in a store with open-ended kinds),
and then `target(id)` is usually the better declaration. `any` demands a
reason, and every use is listed as an exception wherever the table is printed.

An app's own record kinds belong in its `schema-refs.json` before a verb names
them. `canonical` maps each ref to one sentence saying who writes it and who
reads it:

```json
"canonical": {
  "mini-stats/sample": "Written by mini-stats-service_record-sample; read by _list-samples."
}
```

`scripts/new-app.sh` writes `.cargo/config.toml` so that the macros read the
app's copy (`IMPRESS_SCHEMA_REFS`). Without that variable the macros read the
snapshot beside koine's crates, which does not know your kinds:

```
error: `bibliography-entries` is not a canonical schema ref in schema-refs.json; copy the
spelling from the manifest (the store matches `schema_ref` by exact equality, so a
misspelt ref reads zero rows forever)
```

What reads the declaration: `tentatively` answers, without running the verb,
the verb's declared reads, writes and reach beside the policy decision
(grammar.md, adverbs rule 2), and, when a host turns conflict detection on,
the policy layer compares the declared write sets of two destructive calls in
flight. A declaration that is too small
makes a tentative answer lie; one that is too large makes a harmless verb look
dangerous. In the suite, a store spy also checks declarations against what the
handler touched over its examples; a bespoke app can add the same check to its
selftest once it has a store.

## 6. Safety and the other declarations

**`safety`** is declared once per service, `safety = read_only`, and per verb
by exception, `#[impress_method(safety = destructive)]`. It is the worst thing
the verb can do, not what it usually does:

| Class | The verb | What it costs the caller |
|---|---|---|
| `read_only` | reads or computes, changes nothing | nothing: runs under every policy; `within` (a deadline) works with no declaration; idempotent by construction; no audit row |
| `mutating` | writes records or files in a way that can be accounted for | an agent's call is queued for a person under `review-agent-writes`; one audit row per call; say `idempotent = true` if calling twice is one call (an upsert, a `set-*`) |
| `destructive` | deletes or overwrites with no inverse | queued for an agent under `review-agent-writes` and `review-agent-destructive`; the generated form hides Run behind a confirmation toggle; two in flight on the same kind are refused `conflict` when conflict detection is on |
| `external` | leaves the process: a running app, the network, a device, a subprocess, a provider | queued like `destructive`; same confirmation in the form |

The policy is the host's (`run-all` by default); the table is what the class
costs once a host installs a review policy, so choose the class for that day.
Reaching the network does not make a verb `external` unless it does something
there that the store cannot account for.

**`strict_args`** is `true` unless written. A call whose argument object has a
key the input schema does not name is refused `invalid-argument`, naming the
key and the keys the call takes, instead of parsing leniently with a default.
That is what makes a misspelt argument an error rather than a silent default;
leave it on. `strict_args = false` is for a service whose existing callers
still send extra keys. It must be a literal `true` or `false`:

```
error: `strict_args` must be `true` or `false`: it is `true` unless written, and
`false` lets a verb accept argument keys its input schema does not name
```

An argument is always a key of one object (the parameters become the fields of
an args struct), so there is no case of a strict verb whose argument is "not an
object". An argument whose value is free-form takes `serde_json::Value`, and
the check then allows any key inside it.

**`supports(...)`** declares adverbs beyond the universal three
(`tentatively`, `with_review`, `once`, which every verb accepts). Declare one
only if the verb keeps the promise it makes: `within` means dropping the
handler's future mid-flight leaves the store consistent (a read-only verb needs no
declaration); `reversibly` means the handler calls
`impress_service_core::pipeline::context::note_inverse(verb, args)` with the
call that undoes it, or the call fails `effect-failed`; `in_background` is the
host's job runner and needs no code from the verb; `tentatively` declared means
the handler is run with the adverb and must write nothing. Set it for a
service with `supports = [reversibly]` in `impress_service_impl!` or for one
verb with `#[impress_method(supports(reversibly, within))]` (replaces the
service's set). A misspelling is refused with the nearest word:

```
error: unknown adverb `reversbly` (did you mean `reversibly`?); `supports(…)` lists
the adverbs beyond the universal three that this verb honors, one of `tentatively`,
`with_review`, `once`, `reversibly`, `within` or `in_background` (`tentatively`,
`with_review` and `once` need no declaration)
```

**`examples`** are `#[impress_example]` on the trait method (section 3).
`tier = "b"` marks one that needs an isolated running host; leave it off.

**`budget_ms`**, `#[impress_method(budget_ms = 200)]`, is the wall-clock
ceiling, in whole milliseconds, that a Tier A example of this verb may take.
Set it on a verb whose speed is a promise (a search, a compile).

Also available on a method: `idempotent = true | false`,
`deprecated(since = "...", note = "...")` with `aliases = ["old-name"]` for a
rename, `replay = full` to let the call log keep complete arguments, and
`adjective(name = "stale", kinds = ["paper"])` to make a read-only `id` ->
`{holds: bool}` verb a derived adjective (grammar.md).

## 7. The refusals, and what each one says

Each is a case in `crates/impress-service-macros/tests/ui/`, with the message
in the `.stderr` beside it. The macro reports the first one it meets; fix and
rebuild.

| You wrote | The macro says |
|---|---|
| a service with no `effects = { ... }` | `missing effects = { reads: [...], ... }` with an example, and `{}` for a service that touches nothing |
| `supports(reversbly)` | `unknown adverb reversbly (did you mean reversibly?)` and the vocabulary |
| an argument type with no `JsonSchema` or `Deserialize` | at the type: `argument Matrix of a verb needs serde::Deserialize and schemars::JsonSchema`, with the DTO fix |
| a result type with no `JsonSchema` or `Serialize` | at the type: `result Matrix of a verb needs serde::Serialize and schemars::JsonSchema` |
| a method that is not `async` | `must be an async fn: the generated invoker awaits every verb` |
| `fn f<T>(...)` on the trait | `is generic; ... write one method with concrete DTOs per instantiation` |
| `message: &str` | `argument message borrows; ... write the owned type` |
| `strict_args = "yes"` | `strict_args must be true or false` |
| `#[impress_method]` with no `///` | `has no doc comment; write a /// line saying what the verb does` |
| `effects = { reads: ["bibliography-entries"] }` | `is not a canonical schema ref in schema-refs.json; copy the spelling from the manifest` |
| a service with no `safety = ...` or `since = ...` | `missing safety = ...` listing the four classes; `missing since = "<version>"` |

After the first such error rustc may also print follow-on errors from the
derives the macro writes (a missing `Deserialize` is also reported inside
serde); read the first, the one that names a fix.

## 8. The Tier A selftest

Tier A is a test that calls each verb the way a host does: find it in the
linked inventory by name and run it through the pipeline. It never calls the
trait method directly, because that cannot catch a wiring mistake, a missing
example, a strict-argument refusal or a schema that does not describe what the
handler takes.

```rust
use impress_service_core::call;
use serde_json::json;

#[test]
fn mean_through_the_inventory() {
    let result = call::call("stats-service_mean", json!({ "values": [1, 2, 6] })).unwrap();
    assert_eq!(result["value"], json!(3.0));
}

#[test]
fn a_refusal_is_a_result() {
    let result = call::call("stats-service_mean", json!({ "values": [] })).unwrap();
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["code"], json!("empty-input"));
}

#[test]
fn a_misspelt_argument_is_refused() {
    let result = call::call("stats-service_mean", json!({ "vals": [1] })).unwrap();
    assert_eq!(result["code"], json!("invalid-argument"));
}
```

`call::call(name, args)` returns `Err` when the name is not in the inventory
(the service crate is not linked, or the name is wrong), the verb's app is
unreachable, or the handler itself fails; a refusal the verb chose is an `Ok`
value with `ok: false`. `call::descriptors()` lists every linked verb
as a `VerbHandle`, with `.examples()`, `.safety()`, `.effects()`,
`.input_schema()` and `.output_schema()`.

The scaffold's `selftest.rs` carries three generic tests that grow with the
app and need no editing: every verb of the service is documented, every verb
has an `#[impress_example]`, and every example runs and contains its `expect`.
Writing an example is therefore writing a test. Add a named test for each
verb's refusals and each argument shape the examples do not reach.

## 9. What the generated surface shows, and when to write one

`impress-verb-surface` turns each verb's schemas into a form a person can use
in a browser or a terminal, with no code from you:

- one field per argument: text for a `string`, a select for an enum, a
  number, a toggle, a date for a date-shaped `format`, and a raw-JSON text
  field for anything structured; each prefilled from the verb's first example;
- one toggle per adverb the verb accepts;
- a result view chosen by the output schema: a `Vec` of flat objects is a
  table, a flat object a key-value list, a `string` text, anything else raw
  JSON;
- for a `destructive` or `external` verb, Run hidden behind a confirmation.

So the shape of the DTOs is the shape of the page. A descriptive field name,
a `///` on each field and an example per verb are the whole design work for
the generated form.

Author a surface by hand only when the form is not enough: a page that puts
several verbs together (two sliders feeding one verb whose result feeds a
plot), derived state, or a selection in one table that parameterises a call.
A surface is a JSON document that can only compute by calling verbs; there
are no expressions, conditionals or loops in it. The worked example is
`crates/surface-demo-service/examples/surface-demo.surface.json` with its test
`tests/example_surface.rs`, which parses it as a `SurfaceSpec` and runs
`impress_surface::validate` over it; copy that test beside your own surface.
Do not write Swift, Dioxus or HTML for a capability.

## 10. Checklist

Tick every line; each is something you can look at or run.

- [ ] I listed the library's public API (`cargo doc -p <lib> --open`, or the
      TODO list `scripts/new-app.sh` wrote into `service.rs`).
- [ ] Every public function worth calling has one `#[impress_method]`; the ones
      I left out I can name and say why (`scripts/check-verb-coverage.sh --lib
      <crate-dir> --service <service-crate-dir>` lists what has neither a verb
      nor a reasoned line in `verb-exclusions.toml`).
- [ ] Every method is `async fn` and has a `///` that says what it does, what
      it refuses and what the result means.
- [ ] Every argument is an owned type; no reference, lifetime, generic or
      `impl Trait` reaches a signature.
- [ ] Every argument and result type derives `JsonSchema` and `Deserialize`
      (arguments) or `Serialize` (results); each foreign type of the library
      has a DTO with `From`/`TryFrom`.
- [ ] Every argument has a `///` inside `methods = [...]`; secrets carry
      `#[impress_private]`.
- [ ] A verb that can refuse returns a result with `ok`, `code` and `message`
      and refuses with a kebab-case code; no verb returns `Result`.
- [ ] `methods = [...]` lists every verb, with the trait's exact signatures.
- [ ] `effects` is declared (empty lists for pure computation), exceptions are
      per method, every kind is in `schema-refs.json`, and `any("...")` has a
      reason or is gone.
- [ ] `safety` is the worst case of each verb, not the usual one; mutating
      upserts say `idempotent = true`.
- [ ] `strict_args` is left at its default.
- [ ] `supports(...)` names only adverbs the verb keeps the promise of.
- [ ] Every verb has an `#[impress_example]` with an `expect`.
- [ ] `selftest.rs` calls verbs through `call::call`, covers a refusal and a
      misspelt argument, and `cargo test` is green.
- [ ] The host `main` calls `ensure_linked()`, and a running host lists every
      verb of the service.
- [ ] I opened `/ui/` and used one verb's generated form to completion.
