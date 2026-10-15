//! What the notebook verbs compute from stored call records: timeline rows,
//! an entry's flattened steps, per-step comparisons and the Markdown export
//! (plan-koine-notebook N-D7, N-D8). Pure functions over record payloads, so
//! they are tested without a store.

use std::collections::{BTreeMap, BTreeSet};

use impress_service_core::pipeline::context::StepRecord;
use koine_plot::{PlotAxis, PlotColor, PlotSeries, PlotSpec, SeriesStyle};
use serde_json::{Map, Value};

use crate::dto::{CompareRow, StepRow, TimelineRow};
use crate::records::{len, number, text};

/// A payload's steps, parsed. A record written before steps existed has none.
pub fn steps_of(payload: &Map<String, Value>) -> Vec<StepRecord> {
    payload
        .get("steps")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// The record's duration in microseconds: `duration_us` when present, else
/// `duration_ms`.
pub fn duration_us(payload: &Map<String, Value>) -> u64 {
    match payload.get("duration_us").and_then(Value::as_u64) {
        Some(us) => us,
        None => number(payload, "duration_ms").saturating_mul(1000),
    }
}

/// The record as one step: its own verb, time and self time, over its steps.
pub fn root_step(payload: &Map<String, Value>) -> StepRecord {
    StepRecord::new(
        text(payload, "verb").unwrap_or("?"),
        text(payload, "_id").unwrap_or(""),
        duration_us(payload),
        payload.get("ok").and_then(Value::as_bool).unwrap_or(true),
        text(payload, "code").map(str::to_string),
        steps_of(payload),
        number(payload, "steps_truncated") as u32,
    )
}

fn ms(us: u64) -> f64 {
    (us as f64 / 1000.0 * 1000.0).round() / 1000.0
}

/// Who called, as the timeline shows it: `human`, `agent:mcp`, ….
pub fn caller_label(payload: &Map<String, Value>) -> String {
    match payload.get("caller") {
        Some(Value::Object(caller)) => {
            let kind = caller.get("kind").and_then(Value::as_str).unwrap_or("?");
            match caller.get("name").and_then(Value::as_str) {
                Some(name) => format!("{kind}:{name}"),
                None => kind.to_string(),
            }
        }
        _ => text(payload, "_author").unwrap_or("?").to_string(),
    }
}

/// One record as a timeline row.
pub fn timeline_row(payload: &Map<String, Value>, nested: bool) -> TimelineRow {
    let root = root_step(payload);
    TimelineRow {
        call_id: text(payload, "_id").unwrap_or("").to_string(),
        started_at: text(payload, "started_at").unwrap_or("").to_string(),
        verb: root.verb.clone(),
        caller: caller_label(payload),
        ok: root.ok,
        code: root.code.clone(),
        duration_ms: ms(root.duration_us),
        self_ms: ms(root.self_us),
        steps: u64::from(impress_service_core::pipeline::context::count_steps(
            &root.steps,
        )),
        writes: len(payload, "inserted_ids")
            + len(payload, "deleted_ids")
            + len(payload, "updated_ids"),
        reads: len(payload, "read_ids") + number(payload, "reads_truncated"),
        run: text(payload, "run").map(str::to_string),
        nested,
    }
}

/// Whether a record is top-level: no parent, or a parent not in the log.
pub fn is_top_level(payload: &Map<String, Value>, logged: &BTreeSet<String>) -> bool {
    match text(payload, "parent_call") {
        None | Some("") => true,
        Some(parent) => !logged.contains(parent),
    }
}

/// The record's step tree, flattened depth first, the record itself first
/// at depth 0; `label` is the verb indented two spaces per level.
pub fn step_rows(payload: &Map<String, Value>) -> Vec<StepRow> {
    let root = root_step(payload);
    root.flatten()
        .into_iter()
        .enumerate()
        .map(|(index, (depth, step))| StepRow {
            index: index as u64,
            depth: depth as u64,
            label: format!("{}{}", "  ".repeat(depth), step.verb),
            verb: step.verb.clone(),
            call_id: step.call_id.clone(),
            total_ms: ms(step.duration_us),
            self_ms: ms(step.self_us),
            ok: step.ok,
            code: step.code.clone(),
        })
        .collect()
}

/// The `n` steps with the most self time, the record itself included.
pub fn slowest(rows: &[StepRow], n: usize) -> Vec<StepRow> {
    let mut sorted = rows.to_vec();
    sorted.sort_by(|a, b| b.self_ms.total_cmp(&a.self_ms));
    sorted.truncate(n);
    sorted
}

/// Folded stacks of the step tree (`a;b;c <self µs>`), the format inferno and
/// flamegraph.pl read, sorted for a stable diff.
pub fn folded(payload: &Map<String, Value>) -> Vec<String> {
    fn walk(step: &StepRecord, path: &mut Vec<String>, out: &mut BTreeMap<String, u64>) {
        path.push(step.verb.clone());
        *out.entry(path.join(";")).or_insert(0) += step.self_us;
        for child in &step.steps {
            walk(child, path, out);
        }
        path.pop();
    }
    let mut out = BTreeMap::new();
    walk(&root_step(payload), &mut Vec::new(), &mut out);
    out.into_iter()
        .map(|(stack, us)| format!("{stack} {us}"))
        .collect()
}

/// A bar chart of self time per step, in step order (`plot-spec@1.0.0`).
pub fn self_time_plot(rows: &[StepRow]) -> Value {
    let xs: Vec<f64> = rows.iter().map(|r| r.index as f64 + 1.0).collect();
    let ys: Vec<f64> = rows.iter().map(|r| r.self_ms).collect();
    let spec = PlotSpec {
        title: Some("Self time per step".to_string()),
        width: 480.0,
        height: 240.0,
        x_axis: PlotAxis {
            label: Some("step".to_string()),
            ..PlotAxis::default()
        },
        y_axis: PlotAxis {
            label: Some("ms".to_string()),
            ..PlotAxis::default()
        },
        series: vec![PlotSeries::line(xs, ys, "self ms")
            .with_style(SeriesStyle::Bar)
            .with_color(PlotColor::Blue)],
        ..PlotSpec::default()
    };
    serde_json::to_value(spec).unwrap_or(Value::Null)
}

/// Timings of every step, keyed by step verb: `(total µs, self µs)` per
/// occurrence, the record itself included.
pub fn step_timings(payloads: &[Map<String, Value>]) -> BTreeMap<String, Vec<(u64, u64)>> {
    let mut out: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for payload in payloads {
        let root = root_step(payload);
        for (_, step) in root.flatten() {
            out.entry(step.verb.clone())
                .or_default()
                .push((step.duration_us, step.self_us));
        }
    }
    out
}

/// Nearest-rank percentile, `p` in `0..=1`.
fn percentile(values: &[u64], p: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = ((p * sorted.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn mean(values: &[u64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<u64>() as f64 / values.len() as f64
    }
}

/// Per step verb, the timings of set `a` against set `b`.
pub fn compare(a: &[Map<String, Value>], b: &[Map<String, Value>]) -> Vec<CompareRow> {
    let (ta, tb) = (step_timings(a), step_timings(b));
    let verbs: BTreeSet<&String> = ta.keys().chain(tb.keys()).collect();
    verbs
        .into_iter()
        .map(|verb| {
            let empty = Vec::new();
            let sa = ta.get(verb).unwrap_or(&empty);
            let sb = tb.get(verb).unwrap_or(&empty);
            let totals = |s: &[(u64, u64)]| s.iter().map(|t| t.0).collect::<Vec<_>>();
            let selfs = |s: &[(u64, u64)]| s.iter().map(|t| t.1).collect::<Vec<_>>();
            let (a_total, b_total) = (totals(sa), totals(sb));
            let a_p50 = percentile(&a_total, 0.5);
            let b_p50 = percentile(&b_total, 0.5);
            CompareRow {
                step: verb.clone(),
                a_count: sa.len() as u64,
                a_mean_ms: ms(mean(&a_total) as u64),
                a_p50_ms: ms(a_p50),
                a_p95_ms: ms(percentile(&a_total, 0.95)),
                a_self_mean_ms: ms(mean(&selfs(sa)) as u64),
                b_count: sb.len() as u64,
                b_mean_ms: ms(mean(&b_total) as u64),
                b_p50_ms: ms(b_p50),
                b_p95_ms: ms(percentile(&b_total, 0.95)),
                b_self_mean_ms: ms(mean(&selfs(sb)) as u64),
                ratio_p50: if a_p50 > 0 && !sb.is_empty() {
                    Some(((b_p50 as f64 / a_p50 as f64) * 100.0).round() / 100.0)
                } else {
                    None
                },
            }
        })
        .collect()
}

/// A run as Markdown: what was done, by whom, how long each step took, what
/// was written, and the notes (N-D8). `records` oldest first; `notes` maps a
/// call id to its notes' text.
pub fn export_markdown(
    run: &crate::runs::Run,
    records: &[Map<String, Value>],
    notes: &BTreeMap<String, Vec<String>>,
) -> String {
    let mut md = String::new();
    md.push_str(&format!("# Run: {}\n\n", run.name));
    md.push_str(&format!(
        "- Started: {}\n- Ended: {}\n- Host: {}\n- Build: {}\n- Calls: {}\n- Reads recorded: {}\n\n",
        run.started_at,
        run.ended_at.as_deref().unwrap_or("(open)"),
        if run.host.is_empty() { "?" } else { &run.host },
        build_label(&run.build),
        records.len(),
        if run.record_reads { "yes" } else { "no" },
    ));
    md.push_str("| # | Time | Caller | Verb | Outcome | Total ms | Self ms | Steps | Writes |\n");
    md.push_str("|---|---|---|---|---|---:|---:|---:|---:|\n");
    for (i, payload) in records.iter().enumerate() {
        let row = timeline_row(payload, false);
        md.push_str(&format!(
            "| {} | {} | {} | `{}` | {} | {} | {} | {} | {} |\n",
            i + 1,
            row.started_at,
            row.caller,
            row.verb,
            if row.ok {
                "ok".to_string()
            } else {
                row.code.clone().unwrap_or_else(|| "failed".into())
            },
            row.duration_ms,
            row.self_ms,
            row.steps,
            row.writes,
        ));
    }
    let with_detail: Vec<_> = records
        .iter()
        .filter(|p| !steps_of(p).is_empty() || notes.contains_key(text(p, "_id").unwrap_or("")))
        .collect();
    if !with_detail.is_empty() {
        md.push_str("\n## Steps and notes\n");
        for payload in with_detail {
            let id = text(payload, "_id").unwrap_or("");
            md.push_str(&format!(
                "\n### `{}` ({})\n\n",
                text(payload, "verb").unwrap_or("?"),
                id
            ));
            let rows = step_rows(payload);
            if rows.len() > 1 {
                md.push_str("| Step | Total ms | Self ms |\n|---|---:|---:|\n");
                for row in &rows {
                    md.push_str(&format!(
                        "| {}`{}` | {} | {} |\n",
                        "· ".repeat(row.depth as usize),
                        row.verb,
                        row.total_ms,
                        row.self_ms
                    ));
                }
            }
            for note in notes.get(id).into_iter().flatten() {
                md.push_str(&format!("\n> {}\n", note.replace('\n', "\n> ")));
            }
        }
    }
    md
}

fn build_label(build: &Value) -> String {
    match (
        build.get("app").and_then(Value::as_str),
        build.get("version").and_then(Value::as_str),
    ) {
        (Some(app), Some(version)) => format!("{app} {version}"),
        _ => "?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(id: &str, verb: &str, us: u64, steps: Value) -> Map<String, Value> {
        json!({
            "_id": id, "verb": verb, "duration_us": us, "ok": true,
            "steps": steps, "caller": {"kind": "agent", "name": "mcp"},
            "started_at": "2026-10-15T10:00:00.000Z",
            "inserted_ids": ["x"], "read_ids": ["a", "b"], "parent_call": null
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn step(verb: &str, us: u64, self_us: u64) -> Value {
        json!({"verb": verb, "call_id": verb, "duration_us": us, "self_us": self_us,
               "ok": true, "code": null})
    }

    #[test]
    fn a_timeline_row_reads_time_self_time_writes_and_reads() {
        let r = record(
            "c1",
            "x-service_fit",
            10_000,
            json!([step("x-service_load", 6_000, 6_000)]),
        );
        let row = timeline_row(&r, false);
        assert_eq!(row.duration_ms, 10.0);
        assert_eq!(row.self_ms, 4.0);
        assert_eq!(row.steps, 1);
        assert_eq!(row.writes, 1);
        assert_eq!(row.reads, 2);
        assert_eq!(row.caller, "agent:mcp");
    }

    #[test]
    fn steps_flatten_with_depth_and_the_slowest_come_first() {
        let r = record(
            "c1",
            "x-service_fit",
            10_000,
            json!([
                {"verb": "x-service_load", "call_id": "l", "duration_us": 6000, "self_us": 1000,
                 "ok": true, "code": null,
                 "steps": [step("x-service_read", 5000, 5000)]},
            ]),
        );
        let rows = step_rows(&r);
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            ["x-service_fit", "  x-service_load", "    x-service_read"]
        );
        assert_eq!(slowest(&rows, 1)[0].verb, "x-service_read");
        assert_eq!(
            folded(&r),
            vec![
                "x-service_fit 4000",
                "x-service_fit;x-service_load 1000",
                "x-service_fit;x-service_load;x-service_read 5000"
            ]
        );
        let plot: koine_plot::PlotSpec = serde_json::from_value(self_time_plot(&rows)).unwrap();
        assert_eq!(plot.series[0].y, vec![4.0, 1.0, 5.0]);
    }

    #[test]
    fn compare_finds_the_step_that_got_slower() {
        let a = vec![record(
            "a",
            "x-service_fit",
            10_000,
            json!([step("x-service_load", 2_000, 2_000)]),
        )];
        let b = vec![record(
            "b",
            "x-service_fit",
            14_000,
            json!([step("x-service_load", 6_000, 6_000)]),
        )];
        let rows = compare(&a, &b);
        let load = rows.iter().find(|r| r.step == "x-service_load").unwrap();
        assert_eq!(load.ratio_p50, Some(3.0));
        let fit = rows.iter().find(|r| r.step == "x-service_fit").unwrap();
        assert_eq!(fit.a_self_mean_ms, 8.0);
        assert_eq!(fit.b_self_mean_ms, 8.0);
    }

    #[test]
    fn a_record_with_a_logged_parent_is_not_top_level() {
        let mut r = record("c2", "v", 1, json!([]));
        let logged: BTreeSet<String> = ["c1".to_string()].into();
        assert!(is_top_level(&r, &logged));
        r.insert("parent_call".into(), json!("c1"));
        assert!(!is_top_level(&r, &logged));
        r.insert("parent_call".into(), json!("elsewhere"));
        assert!(is_top_level(&r, &logged));
    }
}
