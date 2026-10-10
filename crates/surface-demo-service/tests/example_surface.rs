//! `examples/surface-demo.surface.json` is a real `SurfaceSpec` (ADR-0033 S1)
//! that names this crate's own two verbs and validates clean — the S9
//! "worked example" half of the deliverable.
//!
//! It is a byte-for-byte copy of
//! `crates/impress-surface/examples/signal-explorer.surface.json`: that
//! file's `sources.series`/`sources.hist` already call
//! `surface-demo-service_series`/`surface-demo-service_histogram` with
//! exactly this crate's argument names (`freq`, `n`, `values`, `bins`) and
//! source paths (`{{source.series.values}}`, `{{source.hist.plot}}`), so no
//! field renamed on either side — see this crate's `src/lib.rs` module docs.

use impress_surface::{validate, SurfaceSpec};

const EXAMPLE_JSON: &str = include_str!("../examples/surface-demo.surface.json");

#[test]
fn example_parses_as_a_surface_spec() {
    let spec: SurfaceSpec = serde_json::from_str(EXAMPLE_JSON)
        .unwrap_or_else(|e| panic!("examples/surface-demo.surface.json failed to parse: {e}"));
    assert_eq!(spec.surface, "1.0");
}

#[test]
fn example_validates_with_no_problems() {
    let spec: SurfaceSpec = serde_json::from_str(EXAMPLE_JSON).expect("valid JSON");
    let problems = validate(&spec);
    assert!(problems.is_empty(), "unexpected problems: {problems:?}");
}

#[test]
fn example_names_this_crates_verbs_with_matching_args() {
    let spec: SurfaceSpec = serde_json::from_str(EXAMPLE_JSON).expect("valid JSON");
    let raw: serde_json::Value = serde_json::from_str(EXAMPLE_JSON).expect("valid JSON");
    let _ = &spec; // parsed shape already checked above; args checked on the raw JSON

    let series_args = &raw["sources"]["series"];
    assert_eq!(series_args["verb"], "surface-demo-service_series");
    assert!(series_args["args"].get("freq").is_some());
    assert!(series_args["args"].get("n").is_some());

    let hist_args = &raw["sources"]["hist"];
    assert_eq!(hist_args["verb"], "surface-demo-service_histogram");
    assert!(hist_args["args"].get("values").is_some());
    assert!(hist_args["args"].get("bins").is_some());
    assert_eq!(
        hist_args["args"]["values"], "{{source.series.values}}",
        "hist source must read series' `values` output field"
    );

    // The `plot` widget must point at histogram's `plot` output field.
    let plot_node = &raw["root"]["column"][2]["plot"]["spec"];
    assert_eq!(plot_node, "{{source.hist.plot}}");
}
