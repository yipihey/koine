//! S2: loads every scenario document under the repo's `crates/*/scenarios/`
//! directories (the stored documents S2 converted the Tier B catalogues
//! into), validates each one, and runs the Tier-A-capable ones headlessly
//! against a scratch store. A Tier B document is not run here (it needs a
//! live app, out of scope for a headless `cargo test`) but is still parsed
//! and validated — a malformed stored document fails this test, not a Tier B
//! run nobody schedules in CI.

use std::path::{Path, PathBuf};

use impress_scenario::{validate, Scenario, Tier};
use impress_scenario_service::TierACaller;

/// Force-link every catalogue crate whose `scenarios/` documents this test
/// walks, so a Tier A run's `call` steps (none exist yet as of S2, but a
/// future catalogue's might) resolve in the linked inventory.
#[allow(unused_imports)]
use impress_surface_service as _force_link_impress_surface_service;

/// The repo root, from this crate's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root exists")
}

/// Every `scenarios/*.json` file directly under any `crates/*/scenarios/`
/// directory, sorted for a stable test order.
fn every_scenario_document() -> Vec<PathBuf> {
    let crates_dir = repo_root().join("crates");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&crates_dir).expect("crates/ exists") {
        let entry = entry.expect("readable crates/ entry");
        let scenarios_dir = entry.path().join("scenarios");
        if !scenarios_dir.is_dir() {
            continue;
        }
        for doc in std::fs::read_dir(&scenarios_dir).expect("readable scenarios/ dir") {
            let doc = doc.expect("readable scenarios/ entry");
            if doc.path().extension().and_then(|e| e.to_str()) == Some("json") {
                found.push(doc.path());
            }
        }
    }
    found.sort();
    found
}

fn load(path: &Path) -> Scenario {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not a valid scenario document: {e}", path.display()))
}

#[test]
fn every_stored_scenario_document_is_structurally_valid() {
    let docs = every_scenario_document();
    assert!(
        !docs.is_empty(),
        "expected at least one scenarios/*.json document under crates/*/scenarios/ (S2)"
    );
    for path in &docs {
        let scenario = load(path);
        let problems = validate::validate(&scenario);
        assert!(problems.is_empty(), "{}: {problems:?}", path.display());
        assert_eq!(
            scenario.wire_version,
            1,
            "{}: expected wire_version 1",
            path.display()
        );
    }
}

#[tokio::test]
async fn every_tier_a_document_runs_headlessly() {
    let docs = every_scenario_document();
    let mut ran = 0;
    for path in &docs {
        let scenario = load(path);
        if scenario.tier != Tier::A {
            continue;
        }
        ran += 1;
        let mut caller = TierACaller::open().expect("open scratch store");
        let result = impress_scenario::run(&scenario, &mut caller).await;
        assert!(
            result.pass && !result.skipped,
            "{}: `{}` did not pass: {}",
            path.display(),
            scenario.id,
            result.detail
        );
    }
    // Not an error for zero Tier A documents to exist today (S2 converted
    // Tier B catalogues) — this loop just proves any that do exist actually
    // pass headlessly, the same way `proof_scenarios.rs` does for S1's four.
    let _ = ran;
}
