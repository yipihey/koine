//! S1's row, "the proof": the four converted catalogue entries, run as
//! scenarios in Tier A on a scratch store (docs/plan-self-reflective-layer.md
//! § Scenarios). Three come from `impress-layout-service`'s Tier B
//! catalogue (`layout.apply_preset`, `layout.saved_round_trip`,
//! `layout.wire_contract`); one from `impress-surface-service`
//! (`surface.http.routes`, simplified here to the read-only half a Tier A
//! run can exercise without a live app).
//!
//! Converting every catalogue entry is S2, not S1 — these four are the
//! row's proof that the interpreter and the pipeline runner work end to
//! end, not a replacement for the existing catalogues, which keep running
//! unchanged (`imprint-selftest`/`layout-selftest-service`/
//! `surface-selftest-service`'s own `run_selftest` verbs are untouched by
//! this crate).

use impress_scenario::Scenario;
use impress_scenario_service::TierACaller;
use serde_json::json;

// Force-link the surface-service inventory: this crate's own production
// code never depends on `impress-surface-service`, but a `call` step
// naming one of its verbs needs it linked into whatever binary runs the
// scenario (`impress-capabilities` does this for the real MCP/CLI/HTTP
// paths).
#[allow(unused_imports)]
use impress_surface_service as _force_link_impress_surface_service;

fn parse(spec: serde_json::Value) -> Scenario {
    serde_json::from_value(spec).expect("valid scenario spec")
}

fn layout_apply_preset() -> Scenario {
    parse(json!({
        "wire_version": 1,
        "id": "layout.apply_preset",
        "description": "applying a preset by name sets the live tree",
        "tier": "a",
        "steps": [
            {
                "call": "layout-service_apply-preset",
                "args": {"app_id": "imbib", "name": "Triage"},
                "as": "person",
                "expect": {"ok": true}
            },
            {
                "call": "layout-service_get-layout",
                "args": {"app_id": "imbib"},
                "as": "agent:scenario",
                "expect": {"ok": true, "fields": [{"path": "layout", "present": true}]}
            }
        ],
        "expect_effects": {"writes": ["impress/ui/layout@1.0.0"]}
    }))
}

fn layout_saved_round_trip() -> Scenario {
    parse(json!({
        "wire_version": 1,
        "id": "layout.saved_round_trip",
        "description": "save a layout by name, see it round-trip, delete it",
        "tier": "a",
        "steps": [
            {
                "call": "layout-service_save-layout",
                "args": {"app_id": "imbib", "name": "__scenario-proof-layout__"},
                "as": "person",
                "expect": {"ok": true}
            },
            {
                "call": "layout-service_list-layouts",
                "args": {"app_id": "imbib"},
                "as": "agent:scenario",
                "expect": {"ok": true}
            },
            {
                "call": "layout-service_delete-layout",
                "args": {"app_id": "imbib", "name_or_id": "__scenario-proof-layout__"},
                "as": "person",
                "expect": {"ok": true}
            }
        ]
    }))
}

fn layout_wire_contract() -> Scenario {
    parse(json!({
        "wire_version": 1,
        "id": "layout.wire_contract",
        "description": "an unknown argument field is refused invalid-argument by name",
        "tier": "a",
        "steps": [
            {
                "call": "layout-service_split",
                "args": {
                    "app_id": "imbib",
                    "target": {"role": "detail"},
                    "dir": "horizontal",
                    "not_a_real_field": true
                },
                "as": "agent:scenario",
                "expect": {"ok": false, "code": "invalid-argument"}
            }
        ]
    }))
}

fn surface_http_routes() -> Scenario {
    parse(json!({
        "wire_version": 1,
        "id": "surface.http.routes",
        "description": "the surface spec schema route answers ok with a schema",
        "tier": "a",
        "steps": [
            {
                "call": "impress-surface-service_surface-schema",
                "args": {},
                "as": "agent:scenario",
                "expect": {"ok": true, "fields": [{"path": "schema", "present": true}]}
            },
            {
                "call": "impress-surface-service_surface-list",
                "args": {},
                "as": "agent:scenario",
                "expect": {"ok": true}
            }
        ]
    }))
}

async fn run_and_assert_pass(scenario: &Scenario) {
    let mut caller = TierACaller::open().expect("open scratch store");
    let result = impress_scenario::run(scenario, &mut caller).await;
    assert!(
        result.pass && !result.skipped,
        "scenario `{}` did not pass: {}",
        scenario.id,
        result.detail
    );
}

#[tokio::test]
async fn layout_apply_preset_runs_as_a_scenario() {
    run_and_assert_pass(&layout_apply_preset()).await;
}

#[tokio::test]
async fn layout_saved_round_trip_runs_as_a_scenario() {
    run_and_assert_pass(&layout_saved_round_trip()).await;
}

#[tokio::test]
async fn layout_wire_contract_runs_as_a_scenario() {
    run_and_assert_pass(&layout_wire_contract()).await;
}

#[tokio::test]
async fn surface_http_routes_runs_as_a_scenario() {
    run_and_assert_pass(&surface_http_routes()).await;
}
