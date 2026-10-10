//! The `RenderTree` wire format is pinned.
//!
//! A `RenderTree` is what `surface_render` (S4) returns to an agent and what a
//! renderer (Swift today) maps to pixels one to one (ADR-0033 D2) — its JSON is
//! a compatibility surface the same way the layout tree's is (see
//! `impress-layout/tests/golden.rs`, which this test follows). A deliberate
//! shape change is made by re-blessing the file
//! (`UPDATE_GOLDEN=1 cargo test -p impress-surface --test golden`) *and* saying
//! in the PR what reads the old shape.
//!
//! Both worked examples run through the same test body, table-driven: each
//! case names its spec, a fixed state, and fake `SourceData` standing in for
//! what `impress-surface-service`'s executor would have fetched — this crate
//! never calls a verb or runs a query itself, so every golden is pinned
//! against data a human hand-writes here, never against anything live.

use serde_json::{json, Value};

use impress_surface::{
    example_paper_triage, example_signal_explorer, resolve, RenderTree, SurfaceSpec,
};

/// One golden case: a spec, the state/params it renders with, the fake fetched
/// source data standing in for the executor, and the committed file it is
/// pinned against.
struct Case {
    name: &'static str,
    golden_path: String,
    spec: SurfaceSpec,
    state: Value,
    params: Value,
    source_data: Value,
}

fn golden_path(name: &str) -> String {
    format!(
        "{}/tests/golden/{name}.render.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// The shapes `surface-demo-service_series` / `_histogram` really emit: `x` +
/// `values` for the series, and a `plot-spec@1.0.0` whose `series` is what
/// imprint-core's `FfiPlotSpec` decodes — a synthetic `bars` here kept the
/// Swift golden test from ever exercising the real plot decoder.
fn signal_explorer_fake_data() -> Value {
    json!({
        "series": { "x": [0.0, 0.25, 0.5, 0.75], "values": [0.0, 1.0, 0.0, -1.0] },
        "hist": { "plot": {
            "kind": "plot-spec@1.0.0",
            "title": "Histogram",
            "width": 480.0, "height": 320.0,
            "x": { "label": "value", "scale": "linear", "min": null, "max": null },
            "y": { "label": "count", "scale": "linear", "min": null, "max": null },
            "series": [ { "kind": "line", "color": { "r": 31, "g": 119, "b": 180 },
                          "xs": [-0.75, -0.25, 0.25, 0.75], "ys": [1.0, 2.0, 2.0, 1.0] } ]
        } },
        "papers": [
            { "title": "A dark matter survey", "year": 2024 },
            { "title": "Signal processing notes", "year": 2023 }
        ]
    })
}

/// Fake flattened rows shaped like what `impress-surface-service::runtime`'s
/// `flatten_payloads` really hands `resolve`: envelope fields (`id`, `schema`,
/// `is_read`, `is_starred`, `tags`, `flag`) lifted to the top alongside the
/// `imbib/bibliography-entry` payload fields the table's columns name
/// (`title`, `year`, `author_text` — copied from
/// `crates/imbib-core/src/unified/schemas.rs`'s `bibliography_entry_schema`,
/// never guessed: that schema has no `authors` field, only the single
/// formatted `author_text` string).
fn paper_triage_fake_data() -> Value {
    json!({
        "papers": [
            {
                "id": "11111111-1111-1111-1111-111111111111",
                "schema": "imbib/bibliography-entry",
                "title": "A dark matter survey",
                "year": 2024,
                "author_text": "Zwicky, F.",
                "is_read": false,
                "is_starred": false,
                "tags": [],
                "flag": null
            },
            {
                "id": "22222222-2222-2222-2222-222222222222",
                "schema": "imbib/bibliography-entry",
                "title": "Signal processing notes",
                "year": 2023,
                "author_text": "Shannon, C.",
                "is_read": false,
                "is_starred": false,
                "tags": [],
                "flag": null
            }
        ]
    })
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "signal-explorer",
            golden_path: golden_path("signal-explorer"),
            spec: example_signal_explorer(),
            state: json!({ "freq": 2.0, "bins": 16 }),
            params: json!({}),
            source_data: signal_explorer_fake_data(),
        },
        Case {
            name: "paper-triage",
            golden_path: golden_path("paper-triage"),
            spec: example_paper_triage(),
            // One row pre-selected, so the golden also pins what the status
            // line looks like once a paper has been picked — not just the
            // empty-selection start state `example_paper_triage()` itself
            // declares. `state.selected` is an array of ids now (V5: a
            // widget's `select` event is always an array; the buttons'
            // `each: "state.selected"` fans out over it) — the status line's
            // `{{state.selected}}` reference is unchanged, so this only
            // changes the pinned message text from one id to one id in an
            // array.
            state: json!({ "selected": ["11111111-1111-1111-1111-111111111111"] }),
            params: json!({}),
            source_data: paper_triage_fake_data(),
        },
    ]
}

fn tree_for(case: &Case) -> RenderTree {
    resolve(&case.spec, &case.state, &case.params, &case.source_data)
}

#[test]
fn the_render_tree_serializes_exactly_as_committed() {
    for case in cases() {
        let tree = tree_for(&case);
        let actual = serde_json::to_value(&tree).unwrap();

        if std::env::var("UPDATE_GOLDEN").is_ok() {
            let pretty = serde_json::to_string_pretty(&actual).unwrap();
            std::fs::write(&case.golden_path, format!("{pretty}\n")).unwrap();
        }

        let committed: Value =
            serde_json::from_str(&std::fs::read_to_string(&case.golden_path).expect("golden file"))
                .unwrap();
        assert_eq!(
            actual,
            committed,
            "{}: the RenderTree wire format changed.\nactual:\n{}",
            case.name,
            serde_json::to_string_pretty(&actual).unwrap()
        );
    }
}

#[test]
fn the_committed_json_deserializes_back_to_a_render_tree() {
    for case in cases() {
        let committed: RenderTree =
            serde_json::from_str(&std::fs::read_to_string(&case.golden_path).expect("golden file"))
                .unwrap();
        assert_eq!(committed, tree_for(&case), "{}", case.name);
    }
}
