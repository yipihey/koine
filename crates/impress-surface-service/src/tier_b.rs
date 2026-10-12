//! Surface HTTP catalogue expressed as stored scenarios (SC-1/S2c).
//! The shared Tier B caller preserves the `/api/surface/*` routes, status
//! codes and query arguments. Literal templates are escaped for the surface
//! engine, and the shared interpreter checks cleanup as well as each request.

use crate::report::{CapabilityResult, Tier};
use crate::skipped;
use impress_layout_service::scenario_caller::{LoopbackClient, TierBCaller};
use impress_scenario::Scenario;

pub const BASE_URL_ENV: &str = "IMPRESS_SURFACE_SELFTEST_BASE_URL";
const FALLBACK_ENV: &str = "IMPRESS_LAYOUT_SELFTEST_BASE_URL";
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:23125";

pub fn configured_base_url() -> String {
    [BASE_URL_ENV, FALLBACK_ENV]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

const CATALOGUE: &[(&str, &str)] = &[
    (
        "surface.http.routes",
        "Every /api/surface route answers its verb's result, with wire_version 1",
    ),
    (
        "surface.http.strict",
        "An argument a verb does not take is refused over HTTP, naming it",
    ),
    (
        "surface.http.invalid_spec",
        "An invalid spec is refused at create with every problem, and nothing is stored",
    ),
];

const DOCUMENTS: &[&str] = &[
    include_str!("../scenarios/surface.http.routes.json"),
    include_str!("../scenarios/surface.http.strict.json"),
    include_str!("../scenarios/surface.http.invalid_spec.json"),
];

pub async fn run() -> Vec<CapabilityResult> {
    run_at(&configured_base_url()).await
}

async fn run_at(base: &str) -> Vec<CapabilityResult> {
    if LoopbackClient::new(base).get("/api/surface").await.is_err() {
        return CATALOGUE
            .iter()
            .map(|(id, description)| {
                skipped(
                    id,
                    description,
                    Tier::B,
                    &format!("no app answered on {base}"),
                )
            })
            .collect();
    }
    let mut caller = TierBCaller::for_surface_routes(base);
    let mut results = Vec::new();
    for document in DOCUMENTS {
        let scenario: Scenario = serde_json::from_str(document).expect("embedded surface scenario");
        results.push(impress_scenario::run(&scenario, &mut caller).await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_preserve_catalogue_ids_and_validate() {
        for (document, (id, description)) in DOCUMENTS.iter().zip(CATALOGUE) {
            let scenario: Scenario = serde_json::from_str(document).unwrap();
            assert_eq!(&scenario.id, id);
            assert_eq!(&scenario.description, description);
            assert!(impress_scenario::validate(&scenario).is_empty());
            for step in scenario.steps.iter().chain(&scenario.teardown) {
                if let impress_scenario::Step::Call(call) = step {
                    assert!(
                        impress_service_core::call::find(&call.call).is_some(),
                        "{}",
                        call.call
                    );
                }
            }
        }
    }

    #[test]
    fn layout_surface_scenario_uses_registered_inventory_verbs() {
        let document =
            include_str!("../../impress-layout-service/scenarios/surface.show_and_dispatch.json");
        let scenario: Scenario = serde_json::from_str(document).unwrap();
        assert_eq!(scenario.id, "surface.show_and_dispatch");
        assert!(impress_scenario::validate(&scenario).is_empty());
        for step in scenario.steps.iter().chain(&scenario.teardown) {
            if let impress_scenario::Step::Call(call) = step {
                assert!(
                    impress_service_core::call::find(&call.call).is_some(),
                    "{}",
                    call.call
                );
            }
        }
    }

    #[tokio::test]
    async fn unreachable_host_skips_all_three_cases() {
        let results = run_at("http://127.0.0.1:1").await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.skipped && !r.pass));
    }
}
