//! The performance aggregator: a `tracing` layer that keeps per-name
//! latency buckets for the pipeline's own spans (G7b, plan-auto-gui-and-self-docs.md
//! § Profiling, G7 row).
//!
//! P2's per-call span (`pipeline::mod`, target `"verb"`) already carries
//! `name`, `ok`, `code`, `duration_us`, `result_bytes` — this layer is a
//! second [`tracing_subscriber::Layer`] on the same spans (installed
//! alongside `impress-store-ffi`'s `tracing_bridge`, which only forwards
//! lines to the Console) that folds every closed span into a running bucket
//! keyed `"{target}:{name}"`, e.g. `verb:t-service_echo`. The same scheme
//! covers an `io:*` seam span, should one ever open one (target `"io"`) —
//! today none exists in this workspace (PF-3's `with_write` chokepoint,
//! which would be the first, is deliberately left for a later package), so
//! only `verb:*` buckets are populated in this build.
//!
//! Buckets are exposed as [`PerfBucketStat`] — field-for-field the shape
//! `packages/ImpressLogging/Sources/ImpressLogging/PerfMetrics.swift`
//! already uses, serialized under the same JSON keys (`camelCase`, via
//! `#[serde(rename_all = "camelCase")]`) — so `perf-service_summary`'s rows
//! slot into the Console's Performance tab next to Swift's own, with no
//! second schema. `main_thread_count` is always 0 (the FFI is off the main
//! actor since wave 7 T2); `budget_nanos`/`breach_count` (D-P2, G7c) are
//! `None`/`0` until a `#[impress_method(budget_ms = …)]` call updates them
//! — the pipeline's span carries `budget_ms` only when the verb declares one
//! (`pipeline::mod::prepare`).
//!
//! G7c also keeps a bounded ring of the last [`SPAN_LOG_CAP`] closed spans
//! ([`SpanRecord`], [`trace`]) — the same fields as a bucket, per call
//! rather than aggregated, so `perf-service_trace` can answer with a real
//! tree instead of refusing `unavailable`, and [`trace_chrome_json`]/
//! [`trace_folded_stacks`] can export it.
//!
//! # What this layer must never record
//!
//! Only what the `verb` span itself already carries: name, ok, code,
//! duration, result size. No argument value ever reaches a span field
//! (P7), so no argument value can reach a bucket either.
//!
//! # Self vs total time
//!
//! A call that invokes another call inherits a `parent_call` (nested verb
//! spans exist — see `pipeline::mod`'s identity layer). On close, a span's
//! own duration is added to its bucket's `total`, and to its *parent's*
//! `child_us` counter (tracked in the span's extensions while it is open);
//! `self` is `total - child_us`, tracked per bucket as `self_total_ns`
//! alongside `total_nanos`, and is not currently exposed as its own
//! `PerfBucketStat` field (the Swift shape has none) — it is folded into
//! `total_nanos`/percentiles exactly as `duration_us` always has been,
//! and self-time bookkeeping exists so a later trace-tree export (G7c) has
//! it precomputed rather than needing a second pass over the span log.
//!
//! # Reservoir
//!
//! Each bucket keeps a fixed-size ring of up to [`RESERVOIR_CAP`] recent
//! sample durations; `p50`/`p95` are the nearest-rank percentile over
//! whatever is currently in the ring (matches `PerfMetrics.swift`'s own
//! 1024-sample ring and nearest-rank arithmetic, `:265-270`). `count` and
//! `total_nanos`/`min_nanos`/`max_nanos` are exact over every call ever
//! seen, not just the retained samples.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// The `tracing` targets this layer folds into buckets. `verb` is P2's
/// per-call span; `io` is reserved for a future I/O seam span (none exists
/// in this workspace yet — see the module doc).
pub const AGGREGATED_TARGETS: &[&str] = &["verb", "io"];

/// Samples retained per bucket for the percentile estimate.
pub const RESERVOIR_CAP: usize = 1024;

/// One bucket's live state.
struct Bucket {
    count: u64,
    total_nanos: u64,
    self_total_nanos: u64,
    min_nanos: u64,
    max_nanos: u64,
    /// A ring buffer: `samples[write_pos % cap]`, oldest overwritten first.
    samples: Vec<u64>,
    write_pos: usize,
    /// `#[impress_method(budget_ms = …)]` (D-P2, G7c), in nanoseconds — the
    /// same constant every call of this verb carries, set on the first call
    /// that carries one and never cleared (a `VerbDescriptor` never changes
    /// its budget at runtime).
    budget_nanos: Option<u64>,
    /// How many recorded calls exceeded `budget_nanos`.
    breach_count: u64,
}

impl Bucket {
    fn new() -> Self {
        Bucket {
            count: 0,
            total_nanos: 0,
            self_total_nanos: 0,
            min_nanos: u64::MAX,
            max_nanos: 0,
            samples: Vec::new(),
            write_pos: 0,
            budget_nanos: None,
            breach_count: 0,
        }
    }

    fn record(&mut self, total_nanos: u64, self_nanos: u64, budget_nanos: Option<u64>) {
        self.count += 1;
        self.total_nanos += total_nanos;
        self.self_total_nanos += self_nanos;
        self.min_nanos = self.min_nanos.min(total_nanos);
        self.max_nanos = self.max_nanos.max(total_nanos);
        if self.samples.len() < RESERVOIR_CAP {
            self.samples.push(total_nanos);
        } else {
            self.samples[self.write_pos % RESERVOIR_CAP] = total_nanos;
        }
        self.write_pos = self.write_pos.wrapping_add(1);
        if let Some(budget) = budget_nanos {
            self.budget_nanos = Some(budget);
            if total_nanos > budget {
                self.breach_count += 1;
            }
        }
    }

    /// Nearest-rank percentile (`p` in `0.0..=1.0`) over the retained
    /// samples — the same arithmetic `PerfMetrics.swift` uses.
    fn percentile(&self, p: f64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let rank = ((p * sorted.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        sorted[rank]
    }
}

/// One bucket, snapshotted — field-for-field `PerfBucketStat`
/// (`packages/ImpressLogging/Sources/ImpressLogging/PerfMetrics.swift:47`),
/// serialized under the same JSON keys.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PerfBucketStat {
    pub name: String,
    pub count: u64,
    pub total_nanos: u64,
    pub min_nanos: u64,
    pub max_nanos: u64,
    /// Always 0 for a Rust-side bucket: the FFI is off the main actor
    /// since wave 7 T2.
    pub main_thread_count: u64,
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    /// `#[impress_method(budget_ms = …)]` (D-P2, G7c). `None` until a call
    /// of this verb carries a declared budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_nanos: Option<u64>,
    /// How many recorded calls exceeded `budget_nanos`. `0` when no budget
    /// is declared.
    pub breach_count: u64,
}

impl Bucket {
    fn snapshot(&self, name: &str) -> PerfBucketStat {
        PerfBucketStat {
            name: name.to_string(),
            count: self.count,
            total_nanos: self.total_nanos,
            min_nanos: if self.count == 0 { 0 } else { self.min_nanos },
            max_nanos: self.max_nanos,
            main_thread_count: 0,
            p50_nanos: self.percentile(0.50),
            p95_nanos: self.percentile(0.95),
            budget_nanos: self.budget_nanos,
            breach_count: self.breach_count,
        }
    }
}

static BUCKETS: OnceLock<RwLock<BTreeMap<String, Bucket>>> = OnceLock::new();

fn buckets() -> &'static RwLock<BTreeMap<String, Bucket>> {
    BUCKETS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Every bucket whose key starts with `prefix` (an empty prefix matches
/// all), sorted by key.
pub fn summary(prefix: &str) -> Vec<PerfBucketStat> {
    let guard = buckets().read().unwrap_or_else(|p| p.into_inner());
    guard
        .iter()
        .filter(|(key, _)| key.starts_with(prefix))
        .map(|(key, bucket)| bucket.snapshot(key))
        .collect()
}

/// Clears every bucket and the span log. Test-only — a process has no verb
/// to reset the live aggregator (the point is an honest running history).
#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_test() {
    buckets().write().unwrap_or_else(|p| p.into_inner()).clear();
    span_log()
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
}

// ---------------------------------------------------------------------------
// The span log (G7c): a bounded ring of the last N closed spans, kept for
// trace export. Only what the `verb` span itself carries — name, ids,
// outcome, timing — never an argument value (P7, same rule as the bucket
// table above).
// ---------------------------------------------------------------------------

/// Spans retained in the ring, oldest evicted first.
pub const SPAN_LOG_CAP: usize = 4096;

/// One closed span, as kept for trace export. No argument value is ever
/// captured — only what the `verb`/`io`-target span itself already carries.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpanRecord {
    /// `"{target}:{name}"`, matching a bucket key (e.g. `verb:t-service_echo`).
    pub key: String,
    pub trace_id: String,
    pub call_id: String,
    /// The enclosing call's id, when this call ran nested inside one.
    pub parent_call: Option<String>,
    /// Microseconds since this layer's process-wide epoch (its own first
    /// span open) — a relative clock, good enough to order and offset spans
    /// within one trace; never a wall-clock timestamp.
    pub start_us: u64,
    pub duration_us: u64,
    pub ok: bool,
    pub code: Option<String>,
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

static SPAN_LOG: OnceLock<RwLock<VecDeque<SpanRecord>>> = OnceLock::new();

fn span_log() -> &'static RwLock<VecDeque<SpanRecord>> {
    SPAN_LOG.get_or_init(|| RwLock::new(VecDeque::with_capacity(SPAN_LOG_CAP)))
}

fn push_span_record(record: SpanRecord) {
    let mut log = span_log().write().unwrap_or_else(|p| p.into_inner());
    if log.len() >= SPAN_LOG_CAP {
        log.pop_front();
    }
    log.push_back(record);
}

/// Every recorded span belonging to `trace_id`, oldest first — the raw
/// material for a trace-tree export. Reconstruct the tree from each
/// record's `call_id`/`parent_call`; a trace evicted from the ring (older
/// than the last [`SPAN_LOG_CAP`] calls) returns an empty list, the same
/// shape as a trace id that never existed.
pub fn trace(trace_id: &str) -> Vec<SpanRecord> {
    let log = span_log().read().unwrap_or_else(|p| p.into_inner());
    log.iter()
        .filter(|record| record.trace_id == trace_id)
        .cloned()
        .collect()
}

/// [`trace`] as a Chrome trace-event JSON document — `{"traceEvents": […]}`,
/// one complete (`"ph": "X"`) event per span, `ts`/`dur` in microseconds off
/// this layer's own relative epoch (never a wall-clock timestamp). Opens in
/// Perfetto (ui.perfetto.dev) or `chrome://tracing`. Every span lands on a
/// synthetic thread (`tid`) at or below its nesting depth. Overlapping spans
/// use different rows, including concurrent siblings at the same depth.
pub fn trace_chrome_json(trace_id: &str) -> serde_json::Value {
    let spans = trace(trace_id);
    let depth_of = |record: &SpanRecord, spans: &[SpanRecord]| -> u64 {
        let mut depth = 0u64;
        let mut current = record.parent_call.clone();
        // Bounded by the span log's own cap — no trace can nest deeper than
        // the number of spans it contains.
        while let Some(parent_id) = current {
            let Some(parent) = spans.iter().find(|s| s.call_id == parent_id) else {
                break;
            };
            depth += 1;
            current = parent.parent_call.clone();
        }
        depth
    };
    // A depth alone is not a thread: concurrent siblings have the same depth
    // and can overlap. Allocate the first free row at or below each span's
    // depth, in start-time order, so Chrome never sees overlapping complete
    // events on one synthetic thread.
    let depths: Vec<u64> = spans
        .iter()
        .map(|record| depth_of(record, &spans))
        .collect();
    let mut by_start: Vec<usize> = (0..spans.len()).collect();
    by_start.sort_by_key(|&index| (spans[index].start_us, depths[index]));
    let mut row_end_us: Vec<u64> = Vec::new();
    let mut rows = vec![0usize; spans.len()];
    for index in by_start {
        let record = &spans[index];
        let mut row = depths[index] as usize;
        while row < row_end_us.len() && row_end_us[row] > record.start_us {
            row += 1;
        }
        if row >= row_end_us.len() {
            row_end_us.resize(row + 1, 0);
        }
        row_end_us[row] = record.start_us.saturating_add(record.duration_us);
        rows[index] = row;
    }
    let events: Vec<serde_json::Value> = spans
        .iter()
        .enumerate()
        .map(|(index, record)| {
            serde_json::json!({
                "name": record.key,
                "cat": record.key.split_once(':').map_or("verb", |(target, _)| target),
                "ph": "X",
                "ts": record.start_us,
                "dur": record.duration_us,
                "pid": 1,
                "tid": rows[index],
                "args": {
                    "callId": record.call_id,
                    "parentCall": record.parent_call,
                    "ok": record.ok,
                    "code": record.code,
                },
            })
        })
        .collect();
    serde_json::json!({ "traceEvents": events })
}

/// [`trace`] as folded stacks — one line per distinct root-to-leaf call
/// path, `;`-joined, followed by a space and the number of times that exact
/// path occurred in this trace (the format `inferno`/flamegraph.pl read).
/// Lines are sorted for a stable diff.
pub fn trace_folded_stacks(trace_id: &str) -> Vec<String> {
    let spans = trace(trace_id);
    let path_of = |record: &SpanRecord, spans: &[SpanRecord]| -> String {
        let mut frames = vec![record.key.clone()];
        let mut current = record.parent_call.clone();
        while let Some(parent_id) = current {
            let Some(parent) = spans.iter().find(|s| s.call_id == parent_id) else {
                break;
            };
            frames.push(parent.key.clone());
            current = parent.parent_call.clone();
        }
        frames.reverse();
        frames.join(";")
    };
    let parents: BTreeSet<&str> = spans
        .iter()
        .filter_map(|record| record.parent_call.as_deref())
        .collect();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for record in spans
        .iter()
        .filter(|record| !parents.contains(record.call_id.as_str()))
    {
        *counts.entry(path_of(record, &spans)).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(stack, count)| format!("{stack} {count}"))
        .collect()
}

// ---------------------------------------------------------------------------
// The layer
// ---------------------------------------------------------------------------

/// Fields captured off an aggregated span while it is open: `name` (the
/// bucket key's suffix), the ids the pipeline's span carries, and, at
/// close, `duration_us`/`ok`/`code`.
#[derive(Default, Clone)]
struct SpanFields {
    name: Option<String>,
    duration_us: Option<u64>,
    trace_id: Option<String>,
    call_id: Option<String>,
    parent_call: Option<String>,
    ok: Option<bool>,
    code: Option<String>,
    /// `#[impress_method(budget_ms = …)]` (D-P2, G7c), when the verb
    /// declares one — set on the span by `pipeline::mod::prepare`.
    budget_ms: Option<u64>,
    /// Set at [`Layer::on_new_span`] from the process-wide [`epoch`] — not a
    /// span field, computed rather than parsed off one.
    start_us: Option<u64>,
}

/// A closed child's contribution to its still-open parent's self time.
#[derive(Default)]
struct ChildNanos(AtomicU64);

struct FieldVisitor<'a>(&'a mut SpanFields);

/// A parsed field text, `""` treated as absent — the pipeline's span opens
/// `parent_call`/`code` as `""` when there is none (`Option::as_deref`
/// `.unwrap_or("")`, `pipeline::mod::prepare`/`finish`), so an empty string
/// here means the same "no value" the field itself means.
fn non_empty(text: String) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}").trim_matches('"').to_string();
        match field.name() {
            "name" => self.0.name = non_empty(text),
            "duration_us" => self.0.duration_us = text.parse().ok(),
            "trace_id" => self.0.trace_id = non_empty(text),
            "call_id" => self.0.call_id = non_empty(text),
            "parent_call" => self.0.parent_call = non_empty(text),
            "code" => self.0.code = non_empty(text),
            "ok" => self.0.ok = text.parse().ok(),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "duration_us" => self.0.duration_us = Some(value),
            "budget_ms" => self.0.budget_ms = Some(value),
            _ => {}
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "ok" {
            self.0.ok = Some(value);
        }
    }
}

fn is_aggregated(target: &str) -> bool {
    AGGREGATED_TARGETS.contains(&target)
}

/// The aggregator layer. Install with [`layer`] alongside whatever other
/// `tracing_subscriber::Layer`s the process runs (the FFI's log bridge,
/// an `env-filter` fmt layer, …) — this one never blocks and never talks
/// to a sink; it only updates the in-process bucket table `summary` reads.
pub struct PerfAggregatorLayer;

/// A fresh layer instance. Stateless — the buckets themselves live in a
/// process-wide table (`summary`/`reset_for_test`), so multiple installed
/// instances (e.g. one per test harness within the same process) share one
/// history; that mirrors the audit/log bridges' own process-wide state.
pub fn layer() -> PerfAggregatorLayer {
    PerfAggregatorLayer
}

impl<S> Layer<S> for PerfAggregatorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !is_aggregated(attrs.metadata().target()) {
            return;
        }
        let mut fields = SpanFields::default();
        attrs.record(&mut FieldVisitor(&mut fields));
        fields.start_us = Some(epoch().elapsed().as_micros() as u64);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if !is_aggregated(span.metadata().target()) {
            return;
        }
        let mut ext = span.extensions_mut();
        let Some(fields) = ext.get_mut::<SpanFields>() else {
            return;
        };
        values.record(&mut FieldVisitor(fields));
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let target = span.metadata().target();
        if !is_aggregated(target) {
            return;
        }
        let fields = span
            .extensions()
            .get::<SpanFields>()
            .cloned()
            .unwrap_or_default();
        let Some(name) = fields.name.filter(|n| !n.is_empty()) else {
            return;
        };
        let Some(duration_us) = fields.duration_us else {
            return;
        };
        let total_nanos = duration_us.saturating_mul(1_000);
        let child_nanos = span
            .extensions()
            .get::<ChildNanos>()
            .map(|c| c.0.load(Ordering::Relaxed))
            .unwrap_or(0);
        let self_nanos = total_nanos.saturating_sub(child_nanos);

        let key = format!("{target}:{name}");
        let budget_nanos = fields.budget_ms.map(|ms| ms.saturating_mul(1_000_000));
        buckets()
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entry(key.clone())
            .or_insert_with(Bucket::new)
            .record(total_nanos, self_nanos, budget_nanos);

        // The span log (G7c): only when the span carries the ids the
        // pipeline's own `verb` span always sets (`call_id`/`trace_id`) —
        // an `io`-target span opened by hand in a test may not.
        if let (Some(call_id), Some(trace_id)) = (fields.call_id.clone(), fields.trace_id.clone()) {
            push_span_record(SpanRecord {
                key,
                trace_id,
                call_id,
                parent_call: fields.parent_call.clone(),
                start_us: fields.start_us.unwrap_or(0),
                duration_us,
                ok: fields.ok.unwrap_or(true),
                code: fields.code.clone(),
            });
        }

        if let Some(parent) = span.parent() {
            if is_aggregated(parent.metadata().target()) {
                let mut ext = parent.extensions_mut();
                if let Some(child) = ext.get_mut::<ChildNanos>() {
                    child.0.fetch_add(total_nanos, Ordering::Relaxed);
                } else {
                    ext.insert(ChildNanos(AtomicU64::new(total_nanos)));
                }
            }
        }
    }
}

/// Which `tracing::Level`s this layer needs to see — `INFO`, where the
/// `verb` span is opened (`pipeline::mod`). A `max_level_hint` keeps a
/// filtered subscriber from paying for `DEBUG`/`TRACE` spans this layer
/// never reads.
pub fn max_level_hint() -> Option<Level> {
    Some(Level::INFO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    // `buckets()` is one process-wide table (by design — see the module doc's
    // "process-wide table" note on `layer()`), so these tests race under the
    // default parallel runner: `reset_for_test()` in one test can clear
    // another's in-flight writes, or a concurrently-running test's spans can
    // land in a `summary("")` a sibling test expected to be empty. A
    // module-local mutex serializes just these three tests against each
    // other without reaching for a `serial_test`-style crate dependency for
    // one file.
    static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_layer<F: FnOnce()>(f: F) {
        let subscriber = tracing_subscriber::registry().with(PerfAggregatorLayer);
        tracing::subscriber::with_default(subscriber, f);
    }

    #[test]
    fn a_closed_verb_span_updates_its_bucket() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        with_layer(|| {
            let span = tracing::info_span!(
                target: "verb",
                "verb",
                name = "perf-aggregator-test_alpha",
                ok = tracing::field::Empty,
                code = tracing::field::Empty,
                duration_us = tracing::field::Empty,
                result_bytes = tracing::field::Empty,
            );
            let _entered = span.enter();
            span.record("duration_us", 250u64);
            drop(_entered);
            drop(span);
        });
        let rows = summary("verb:perf-aggregator-test_alpha");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 1);
        assert_eq!(rows[0].total_nanos, 250_000);
        assert_eq!(rows[0].min_nanos, 250_000);
        assert_eq!(rows[0].max_nanos, 250_000);
        assert_eq!(rows[0].p50_nanos, 250_000);
        assert_eq!(rows[0].main_thread_count, 0);
        assert_eq!(rows[0].breach_count, 0);
        assert!(rows[0].budget_nanos.is_none());
    }

    #[test]
    fn a_prefix_filters_the_summary() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        with_layer(|| {
            for n in 0..3 {
                let span = tracing::info_span!(
                    target: "verb",
                    "verb",
                    name = "perf-aggregator-test_beta",
                    ok = tracing::field::Empty,
                    code = tracing::field::Empty,
                    duration_us = tracing::field::Empty,
                    result_bytes = tracing::field::Empty,
                );
                let _entered = span.enter();
                span.record("duration_us", 10u64 + n);
                drop(_entered);
                drop(span);
            }
        });
        let all = summary("");
        assert!(all
            .iter()
            .any(|b| b.name == "verb:perf-aggregator-test_beta"));
        let none = summary("io:perf-aggregator-test_beta");
        assert!(none.is_empty());
        let row = summary("verb:perf-aggregator-test_beta")
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(row.count, 3);
        assert_eq!(row.total_nanos, (10 + 11 + 12) * 1_000);
    }

    #[test]
    fn a_span_outside_the_aggregated_targets_is_ignored() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        with_layer(|| {
            let span = tracing::info_span!(target: "layout", "layout-span");
            let _entered = span.enter();
            drop(_entered);
            drop(span);
        });
        assert!(summary("").is_empty());
    }

    /// Opens one `verb`-target span with a given name/trace/call/parent —
    /// the shape `pipeline::mod::prepare` builds, minus the layers this
    /// module doesn't need (policy, identity).
    #[allow(clippy::too_many_arguments)]
    fn open_span(
        name: &str,
        trace_id: &str,
        call_id: &str,
        parent_call: &str,
        duration_us: u64,
        ok: bool,
        budget_ms: Option<u64>,
    ) {
        let span = tracing::info_span!(
            target: "verb",
            "verb",
            name = name,
            trace_id = trace_id,
            call_id = call_id,
            parent_call = parent_call,
            ok = tracing::field::Empty,
            code = tracing::field::Empty,
            duration_us = tracing::field::Empty,
            result_bytes = tracing::field::Empty,
            budget_ms = tracing::field::Empty,
        );
        let _entered = span.enter();
        if let Some(budget_ms) = budget_ms {
            span.record("budget_ms", budget_ms);
        }
        span.record("ok", ok);
        span.record("duration_us", duration_us);
        drop(_entered);
        drop(span);
    }

    #[test]
    fn trace_returns_the_recorded_spans_for_one_trace_id() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        with_layer(|| {
            open_span(
                "perf-aggregator-test_root",
                "trace-1",
                "call-root",
                "",
                100,
                true,
                None,
            );
            open_span(
                "perf-aggregator-test_other-trace",
                "trace-2",
                "call-x",
                "",
                50,
                true,
                None,
            );
        });
        let spans = trace("trace-1");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].key, "verb:perf-aggregator-test_root");
        assert_eq!(spans[0].call_id, "call-root");
        assert!(spans[0].parent_call.is_none());
        assert_eq!(spans[0].duration_us, 100);
        assert!(trace("trace-that-never-happened").is_empty());
    }

    #[test]
    fn a_nested_trace_exports_as_chrome_json_and_folded_stacks() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        with_layer(|| {
            // Child closes before its still-open parent, as a real nested
            // call does (`pipeline::invoke_with` awaits the inner call
            // before the outer span's own `finish`).
            open_span(
                "perf-aggregator-test_child",
                "trace-nest",
                "call-child",
                "call-parent",
                40,
                true,
                None,
            );
            open_span(
                "perf-aggregator-test_parent",
                "trace-nest",
                "call-parent",
                "",
                100,
                true,
                None,
            );
        });

        let json = trace_chrome_json("trace-nest");
        let events = json["traceEvents"].as_array().expect("traceEvents array");
        assert_eq!(events.len(), 2);
        let child = events
            .iter()
            .find(|e| e["name"] == "verb:perf-aggregator-test_child")
            .expect("child event");
        assert_eq!(child["ph"], "X");
        assert_eq!(child["dur"], 40);
        assert_eq!(child["tid"], 1, "nested one level under its parent");
        let parent = events
            .iter()
            .find(|e| e["name"] == "verb:perf-aggregator-test_parent")
            .expect("parent event");
        assert_eq!(parent["tid"], 0, "the root of this trace");

        let folded = trace_folded_stacks("trace-nest");
        assert_eq!(
            folded,
            vec!["verb:perf-aggregator-test_parent;verb:perf-aggregator-test_child 1".to_string()]
        );
    }

    #[test]
    fn concurrent_siblings_get_distinct_chrome_rows() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        push_span_record(SpanRecord {
            key: "verb:parent".into(),
            trace_id: "parallel".into(),
            call_id: "parent".into(),
            parent_call: None,
            start_us: 0,
            duration_us: 100,
            ok: true,
            code: None,
        });
        for (call_id, start_us) in [("child-a", 10), ("child-b", 20)] {
            push_span_record(SpanRecord {
                key: "verb:child".into(),
                trace_id: "parallel".into(),
                call_id: call_id.into(),
                parent_call: Some("parent".into()),
                start_us,
                duration_us: 50,
                ok: true,
                code: None,
            });
        }
        let chrome = trace_chrome_json("parallel");
        let events = chrome["traceEvents"].as_array().unwrap();
        let first = events
            .iter()
            .find(|event| event["args"]["callId"] == "child-a")
            .unwrap();
        let second = events
            .iter()
            .find(|event| event["args"]["callId"] == "child-b")
            .unwrap();
        assert_ne!(first["tid"], second["tid"]);
        assert_eq!(
            trace_folded_stacks("parallel"),
            vec!["verb:parent;verb:child 2"]
        );
    }

    #[test]
    fn a_budget_breach_is_counted_and_a_call_inside_budget_is_not() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_test();
        with_layer(|| {
            open_span(
                "perf-aggregator-test_budgeted",
                "trace-budget-1",
                "call-1",
                "",
                4_000,
                true,
                Some(5), // 5ms = 5,000us budget; this call is inside it
            );
            open_span(
                "perf-aggregator-test_budgeted",
                "trace-budget-2",
                "call-2",
                "",
                9_000,
                true,
                Some(5), // over the same 5ms budget
            );
        });
        let row = summary("verb:perf-aggregator-test_budgeted")
            .into_iter()
            .next()
            .expect("one bucket");
        assert_eq!(row.count, 2);
        assert_eq!(row.budget_nanos, Some(5_000_000));
        assert_eq!(row.breach_count, 1, "only the 9ms call breached");
    }
}
