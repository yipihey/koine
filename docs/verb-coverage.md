# Verb coverage: what is exposed, and what a library has not yet exposed

**Decided by:** `docs/plan-koine-host.md` H-D7.
**Enforced by:** `scripts/check-verb-coverage.sh` (read-only; bash plus
`python3`, no cargo build). `scripts/tests/coverage/run.sh` tests the script.

Two questions, one script. *Is every service this repository ships recorded
and honest?* is the default mode. *Did I expose everything the library can
do?* is `--lib`.

## Services

The table between the markers is data: edit it and the check changes with it.
There is no second list.

<!-- verb-coverage:begin -->
| Service | Crate | Verbs |
|---|---|---:|
| `surface-demo-service` | surface-demo-service | 11 |
| `layout-service` | impress-layout-service | 42 |
| `layout-selftest-service` | impress-layout-service | 1 |
| `impress-surface-service` | impress-surface-service | 15 |
| `surface-selftest-service` | impress-surface-service | 1 |
| `notebook-service` | koine-notebook | 10 |
| `history-service` | koine-notebook | 7 |
| `agent-service` | koine-agent | 12 |
| `package-service` | koine-packages | 15 |
| `task-service` | koine-tasks | 5 |
| `review-service` | koine-tasks | 6 |
| `impress-ai-service` | koine-ai | 17 |
| `perf-service` | perf-service | 4 |
| `capabilities-service` | capabilities-service | 4 |
| `triage-service` | koine-triage | 5 |
| `collection-service` | koine-collections | 12 |
| `settings-service` | koine-settings-service | 6 |
| `impress-scenario-service` | impress-scenario-service | 6 |
| `impress-workflow-service` | impress-workflow-service | 7 |
| `memory-service` | koine-memory | 7 |
| `store-query-service` | koine-store-query | 4 |
| `provider-service` | koine-providers | 2 |
| `source-service` | source-service | 6 |
<!-- verb-coverage:end -->

`impress-settings` is the registry library and has no `impress_service_impl!`
block; the settings verbs are `koine-settings-service`'s.

## Default mode

```bash
scripts/check-verb-coverage.sh
```

For every `impress_service_impl! { service = X, ... }` block under
`crates/*/src` (comments and strings are blanked first, so a doc example is
not a block): the service has a row above (kebab-cased `X`), the block
declares `effects = {` (ADR-0036 D1), the row's crate and verb count match the
`#[impress_service] pub trait X` it implements (`#[impress_method]` count),
and the block's `methods = [ ... ]` list is as long as the trait. Every row
must name a block. Each mismatch is a `FAIL:` line; the exit status is
non-zero if there is one. `--root <dir>` runs it against another
koine-shaped repository.

## Library mode (H-D7)

```bash
scripts/check-verb-coverage.sh --lib crates/impress-layout
scripts/check-verb-coverage.sh --lib crates/impress-layout --service crates/impress-layout-service
scripts/check-verb-coverage.sh --lib ... --service ... --report-only
```

`--lib <crate-dir>` alone prints the library's **public surface**: what is
there to expose, which is the agent's first question. The listing is a
**source heuristic, not rustc**. It scans `src/**/*.rs` (not `main.rs` or
`bin/`) for `pub fn`, `pub async fn`, `pub struct`, `pub enum` and
`pub trait` at module level, and `pub fn` inside inherent `impl` blocks of
`pub` types. It skips `pub(crate)`/`pub(super)` items, `#[cfg(test)]`
modules, `#[doc(hidden)]` items and trait impls; it does not follow `pub use`
re-exports, expand macros, or list a trait's methods (the trait is the item).
Private modules' `pub` items are listed, since they are usually re-exported.

With `--service <service-crate-dir>` each item is matched to a verb of that
crate's `#[impress_method]`s (read from `src/**/*.rs`, or the directory's own
`.rs` files when it has no `src/`), in this order:

1. a `# covers: Type::method` line (comma-separated for several) in the doc
   comment above the method;
2. snake-case name equality: `Type::method`, a free `fn name`, or a type
   `FooBar` against verb `foo_bar`;
3. an entry in `<service-crate-dir>/verb-exclusions.toml`:

   ```toml
   [[exclude]]
   item = "Type::method"   # or a bare name for a free fn or a type
   reason = "why an agent has no use for it"
   ```

The output is three lists: `covered`, `excluded` (with the reason) and
`UNCOVERED`. The script exits 1 while UNCOVERED is non-empty, saying what to
do; `--report-only` prints the same lists and exits 0. An exclusion that names
no public item is a warning on stderr (the library moved on).
