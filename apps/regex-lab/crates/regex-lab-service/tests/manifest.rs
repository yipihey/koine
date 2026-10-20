//! This workspace builds koine's crates by path, and `impress-service-macros`
//! checks every `effects` ref against the manifest beside the workspace that
//! builds it — this one, not koine's. So this manifest must carry every
//! canonical ref koine's does (plus regex-lab's own), or a kind koine adds
//! breaks this build with a "not a canonical schema ref" error far from the
//! cause, as `koine/pending-call@1.0.0` did on 2026-10-18.

use std::collections::BTreeSet;
use std::path::Path;

fn canonical(path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&text).expect("schema-refs.json is JSON");
    json["canonical"]
        .as_object()
        .expect("canonical is an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn this_manifest_carries_every_canonical_ref_koine_has() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ours = canonical(&here.join("../../schema-refs.json"));
    let koine = canonical(&here.join("../../../../schema-refs.json"));
    let missing: Vec<_> = koine.difference(&ours).collect();
    assert!(
        missing.is_empty(),
        "apps/regex-lab/schema-refs.json lacks koine's canonical refs {missing:?}; copy their \
         entries from koine/schema-refs.json (the macros check effects refs against the \
         manifest beside the workspace that builds them)"
    );
}
