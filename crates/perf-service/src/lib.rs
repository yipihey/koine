//! `PerfService` — the agent-facing read of the pipeline's own performance
//! aggregator (G7b/G7c, plan-auto-gui-and-self-docs.md § Profiling, G7 row,
//! D-G2).
//!
//! Every verb here is a thin, read-only wrapper over
//! `impress_service_core::pipeline::perf`: `summary` returns the bucket
//! table [`perf::summary`] keeps, field-for-field `PerfBucketStat`
//! (`packages/ImpressLogging/Sources/ImpressLogging/PerfMetrics.swift:47`)
//! so the Console's Performance tab can show a Rust row next to a Swift one
//! with no second schema, now including `budgetNanos`/`breachCount` once a
//! verb declares `#[impress_method(budget_ms = …)]` (D-P2). `trace` returns
//! the recorded span tree for one trace id from the aggregator's bounded
//! span log (G7c, `perf::trace`); `export-chrome-trace` and
//! `export-folded-stacks` turn that same tree into a Chrome trace-event
//! document (opens in Perfetto/`chrome://tracing`) or one folded-stack line
//! per call path (the format `inferno`/flamegraph.pl read), for a trace too
//! big to read as a flat list.
//!
//! No verb here takes or returns an argument *value* from a verb call —
//! only names, ids, outcomes and durations the pipeline's own `verb`-target
//! span already carries (P7: a span records the verb name, ids, `ok`,
//! `code`, sizes and durations only, never an argument). `summary`'s
//! `prefix` and every verb's `trace_id` are operator-supplied filters,
//! never echoed anywhere but that call's own result.

use impress_service_core::async_trait;
use impress_service_core::pipeline::perf;
use impress_service_macros::{impress_service, impress_service_impl};

#[allow(unused_imports)]
use impress_service_macros::{impress_example, impress_method};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use perf::{PerfBucketStat, SpanRecord};

/// Result of [`PerfService::summary`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerfSummaryResult {
    pub ok: bool,
    /// One row per bucket whose key starts with the call's `prefix`
    /// (default `""`, matching everything), sorted by key
    /// (`"verb:<name>"` or `"io:<name>"`).
    pub buckets: Vec<PerfBucketStat>,
}

/// Result of [`PerfService::trace`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerfTraceResult {
    pub ok: bool,
    /// Every recorded span belonging to this trace, oldest first. Empty
    /// both for a trace id that never happened and for one that scrolled
    /// out of the ring (`perf::SPAN_LOG_CAP` calls ago) — this verb never
    /// distinguishes the two, the same "no rows, not an error" contract
    /// `summary` already has.
    pub spans: Vec<SpanRecord>,
}

/// Result of [`PerfService::export_chrome_trace`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerfChromeTraceResult {
    pub ok: bool,
    /// `{"traceEvents": […]}` — open this in Perfetto (ui.perfetto.dev) or
    /// `chrome://tracing`. Empty `traceEvents` for a trace id with no
    /// recorded spans.
    pub trace: serde_json::Value,
}

/// Result of [`PerfService::export_folded_stacks`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerfFoldedStacksResult {
    pub ok: bool,
    /// One line per distinct root-to-leaf call path in this trace,
    /// `;`-joined, followed by a space and how many times that exact path
    /// occurred — the format `inferno`/flamegraph.pl read. Empty for a
    /// trace id with no recorded spans.
    pub lines: Vec<String>,
}

/// `PerfService` — a read of the in-process performance aggregator that
/// folds the pipeline's own `verb`-target spans into per-name latency
/// buckets and a bounded per-call span log. Never records or returns an
/// argument value.
#[impress_service]
pub trait PerfService: Send + Sync + 'static {
    /// The aggregator's bucket rows: count, total/min/max/p50/p95
    /// duration, keyed `"verb:<name>"` (and `"io:<name>"` for an I/O seam
    /// span, once one exists), plus `budgetNanos`/`breachCount` for a verb
    /// that declares `#[impress_method(budget_ms = …)]` (D-P2). Pass
    /// `prefix` to narrow the rows returned, e.g. `"verb:imbib"` for one
    /// service's verbs; the default (empty) returns every bucket. A fresh
    /// process with no verb calls yet returns an empty list, not an error.
    #[impress_method(safety = read_only)]
    #[impress_example(name = "default", args = r#"{}"#)]
    #[impress_example(name = "prefixed", args = r#"{"prefix": "verb:perf-service"}"#)]
    async fn summary(&self, prefix: Option<String>) -> PerfSummaryResult;

    /// The recorded span tree for one trace id, from the aggregator's
    /// bounded span log (the last `perf::SPAN_LOG_CAP` closed spans across
    /// every trace, so an old or never-existent trace id answers with an
    /// empty list, not an error). Reconstruct the tree from each span's
    /// `callId`/`parentCall`, or ask `export-chrome-trace`/
    /// `export-folded-stacks` to do it for you.
    #[impress_method(safety = read_only)]
    #[impress_example(
        name = "unknown-trace",
        args = r#"{"trace_id": "00000000-0000-0000-0000-000000000000"}"#,
        expect = r#"{"ok": true, "spans": []}"#
    )]
    async fn trace(&self, trace_id: String) -> PerfTraceResult;

    /// One trace id's span tree as Chrome trace-event JSON
    /// (`{"traceEvents": […]}`) — open it in Perfetto (ui.perfetto.dev) or
    /// `chrome://tracing`. `ts`/`dur` are microseconds on the aggregator's
    /// own relative clock, never a wall-clock timestamp; nested calls land
    /// on separate synthetic rows (`tid`) so a caller and its callee never
    /// overlap on one row.
    #[impress_method(safety = read_only)]
    #[impress_example(
        name = "unknown-trace",
        args = r#"{"trace_id": "00000000-0000-0000-0000-000000000000"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn export_chrome_trace(&self, trace_id: String) -> PerfChromeTraceResult;

    /// One trace id's span tree as folded stacks — one line per distinct
    /// root-to-leaf call path plus its occurrence count, the format
    /// `inferno`/flamegraph.pl turn into a flame graph.
    #[impress_method(safety = read_only)]
    #[impress_example(
        name = "unknown-trace",
        args = r#"{"trace_id": "00000000-0000-0000-0000-000000000000"}"#,
        expect = r#"{"ok": true, "lines": []}"#
    )]
    async fn export_folded_stacks(&self, trace_id: String) -> PerfFoldedStacksResult;
}

/// The default (and only) implementation: reads
/// [`impress_service_core::pipeline::perf`]'s process-wide bucket table and
/// span log. Holds no state of its own — the aggregator is already
/// process-wide, kept by the installed `tracing` layer
/// (`impress-store-ffi`'s log-sink install, `impress-mcp`/`impress-cli`'s
/// `main`), not by this service.
#[derive(Clone, Default)]
pub struct DefaultPerfService;

impl DefaultPerfService {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl PerfService for DefaultPerfService {
    async fn summary(&self, prefix: Option<String>) -> PerfSummaryResult {
        let buckets = perf::summary(prefix.as_deref().unwrap_or(""));
        PerfSummaryResult { ok: true, buckets }
    }

    async fn trace(&self, trace_id: String) -> PerfTraceResult {
        PerfTraceResult {
            ok: true,
            spans: perf::trace(&trace_id),
        }
    }

    async fn export_chrome_trace(&self, trace_id: String) -> PerfChromeTraceResult {
        PerfChromeTraceResult {
            ok: true,
            trace: perf::trace_chrome_json(&trace_id),
        }
    }

    async fn export_folded_stacks(&self, trace_id: String) -> PerfFoldedStacksResult {
        PerfFoldedStacksResult {
            ok: true,
            lines: perf::trace_folded_stacks(&trace_id),
        }
    }
}

impress_service_impl! {
    service = PerfService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [],
        writes: [],
        reach: [],
    },
    impl = DefaultPerfService,
    instance = DefaultPerfService::new,
    strict_args = true,
    methods = [
        /// The aggregator's bucket rows: count, total/min/max/p50/p95
        /// duration, keyed `"verb:<name>"` (and `"io:<name>"` once an I/O
        /// seam span exists), plus `budgetNanos`/`breachCount` once a verb
        /// declares one. Narrow with `prefix`; empty (default) returns
        /// every bucket.
        summary(
            /// Optional bucket-key prefix; omitted or empty returns every bucket.
            prefix: Option<String>
        ) -> PerfSummaryResult,
        /// The recorded span tree for one trace id, from the bounded span
        /// log. Empty for an unknown or evicted trace id, never an error.
        trace(
            /// Trace identifier from a verb call; unknown or evicted traces are empty.
            trace_id: String
        ) -> PerfTraceResult,
        /// One trace id's span tree as Chrome trace-event JSON, for
        /// Perfetto/`chrome://tracing`.
        export_chrome_trace(
            /// Trace identifier from a verb call; unknown or evicted traces are empty.
            trace_id: String
        ) -> PerfChromeTraceResult,
        /// One trace id's span tree as folded stacks, for
        /// `inferno`/flamegraph.pl.
        export_folded_stacks(
            /// Trace identifier from a verb call; unknown or evicted traces are empty.
            trace_id: String
        ) -> PerfFoldedStacksResult,
    ],
}

#[cfg(test)]
mod tests {
    use impress_service_core::pipeline::{identity::CallerIdentity, invoke_blocking, Call};
    use impress_service_core::McpToolDescriptor;
    use serde_json::json;
    use std::sync::Once;

    /// The pipeline's `verb` span is a no-op with no `tracing` subscriber
    /// installed — this test binary installs the same aggregator layer
    /// `impress-store-ffi`'s `install()` and the MCP/CLI `main`s install in
    /// a real process, once per process, so `perf::summary` sees the calls
    /// this test makes through the linked inventory.
    fn ensure_subscriber() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            use tracing_subscriber::layer::SubscriberExt;
            let subscriber =
                tracing_subscriber::registry().with(impress_service_core::pipeline::perf::layer());
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    /// Calls `name` the way MCP, the CLI and impel actually do: through
    /// `pipeline::invoke` — not `tool.handler` directly, which bypasses the
    /// pipeline entirely (span, policy, audit) and so would never populate
    /// the aggregator this crate reads.
    fn call(name: &str, args: serde_json::Value) -> serde_json::Value {
        ensure_subscriber();
        let tool = McpToolDescriptor::iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} should be registered in the inventory"));
        invoke_blocking(tool.verb, Call::new(CallerIdentity::agent("test"), args))
            .unwrap_or_else(|e| panic!("{name} failed: {e:?}"))
    }

    /// The proof the work package asks for: after a few verbs run through
    /// the pipeline (this test's own two calls, at minimum — `summary`
    /// and `trace` are themselves invoked through the linked inventory,
    /// so they are `verb:*` spans too by the time `summary` reads them),
    /// `perf-service_summary` lists `verb:*` rows with nonzero counts.
    #[test]
    fn summary_lists_verb_rows_with_nonzero_counts_after_a_few_calls() {
        // Warm the aggregator: a handful of real inventory calls, through
        // the pipeline, the same way MCP/CLI/impel dispatch — not the trait
        // directly, which would bypass the `verb`-target span entirely.
        for _ in 0..3 {
            call("perf-service_trace", json!({"trace_id": "warm-up"}));
        }

        let result = call(
            "perf-service_summary",
            json!({"prefix": "verb:perf-service"}),
        );
        assert_eq!(result["ok"], json!(true));
        let buckets = result["buckets"].as_array().expect("buckets array");
        assert!(
            !buckets.is_empty(),
            "expected at least one verb:* bucket after warming the aggregator, got {buckets:?}"
        );
        let trace_bucket = buckets
            .iter()
            .find(|b| b["name"] == json!("verb:perf-service_trace"))
            .unwrap_or_else(|| panic!("no verb:perf-service_trace bucket in {buckets:?}"));
        assert!(
            trace_bucket["count"].as_u64().unwrap_or(0) >= 3,
            "expected count >= 3, got {trace_bucket:?}"
        );
        assert!(trace_bucket["totalNanos"].as_u64().unwrap_or(0) > 0);
        assert_eq!(trace_bucket["mainThreadCount"], json!(0));
        assert_eq!(trace_bucket["breachCount"], json!(0));
        assert!(trace_bucket.get("budgetNanos").is_none());
    }

    #[test]
    fn trace_answers_empty_not_an_error_for_an_unknown_trace_id() {
        let result = call(
            "perf-service_trace",
            json!({"trace_id": "00000000-0000-0000-0000-000000000000"}),
        );
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["spans"], json!([]));
    }

    /// The proof G7c asks for: a call made with a known `trace_id` (via
    /// `Call::with_trace`, the shape `traceparent`-carrying transports use)
    /// shows up in `perf-service_trace` for that id, with no argument value
    /// anywhere in the answer.
    #[test]
    fn trace_returns_the_span_for_a_call_made_with_a_known_trace_id() {
        ensure_subscriber();
        let tool = McpToolDescriptor::iter()
            .find(|t| t.name == "perf-service_summary")
            .expect("perf-service_summary is registered");
        invoke_blocking(
            tool.verb,
            Call::new(CallerIdentity::agent("test"), json!({})).with_trace("g7c-known-trace-id"),
        )
        .expect("perf-service_summary should succeed");

        let result = call(
            "perf-service_trace",
            json!({"trace_id": "g7c-known-trace-id"}),
        );
        assert_eq!(result["ok"], json!(true));
        let spans = result["spans"].as_array().expect("spans array");
        assert!(
            spans
                .iter()
                .any(|s| s["key"] == json!("verb:perf-service_summary")),
            "expected a verb:perf-service_summary span in {spans:?}"
        );
        for span in spans {
            assert_eq!(span["traceId"], json!("g7c-known-trace-id"));
        }
    }

    #[test]
    fn export_chrome_trace_and_folded_stacks_answer_for_a_known_trace_id() {
        ensure_subscriber();
        let tool = McpToolDescriptor::iter()
            .find(|t| t.name == "perf-service_summary")
            .expect("perf-service_summary is registered");
        invoke_blocking(
            tool.verb,
            Call::new(CallerIdentity::agent("test"), json!({})).with_trace("g7c-export-trace-id"),
        )
        .expect("perf-service_summary should succeed");

        let chrome = call(
            "perf-service_export-chrome-trace",
            json!({"trace_id": "g7c-export-trace-id"}),
        );
        assert_eq!(chrome["ok"], json!(true));
        let events = chrome["trace"]["traceEvents"]
            .as_array()
            .expect("traceEvents array");
        assert!(events
            .iter()
            .any(|e| e["name"] == json!("verb:perf-service_summary")));

        let folded = call(
            "perf-service_export-folded-stacks",
            json!({"trace_id": "g7c-export-trace-id"}),
        );
        assert_eq!(folded["ok"], json!(true));
        let lines = folded["lines"].as_array().expect("lines array");
        assert!(lines.iter().any(|l| l
            .as_str()
            .unwrap_or("")
            .starts_with("verb:perf-service_summary")));
    }

    #[test]
    fn export_verbs_answer_empty_not_an_error_for_an_unknown_trace_id() {
        let chrome = call(
            "perf-service_export-chrome-trace",
            json!({"trace_id": "00000000-0000-0000-0000-000000000000"}),
        );
        assert_eq!(chrome["ok"], json!(true));
        assert_eq!(chrome["trace"]["traceEvents"], json!([]));

        let folded = call(
            "perf-service_export-folded-stacks",
            json!({"trace_id": "00000000-0000-0000-0000-000000000000"}),
        );
        assert_eq!(folded["ok"], json!(true));
        assert_eq!(folded["lines"], json!([]));
    }

    #[test]
    fn an_empty_prefix_matches_every_bucket() {
        call("perf-service_trace", json!({"trace_id": "prefix-test"}));
        let all = call("perf-service_summary", json!({}));
        let scoped = call(
            "perf-service_summary",
            json!({"prefix": "verb:perf-service_trace"}),
        );
        let all_buckets = all["buckets"].as_array().unwrap();
        let scoped_buckets = scoped["buckets"].as_array().unwrap();
        assert!(all_buckets.len() >= scoped_buckets.len());
        assert!(scoped_buckets.iter().all(|b| b["name"]
            .as_str()
            .unwrap()
            .starts_with("verb:perf-service_trace")));
    }
}
