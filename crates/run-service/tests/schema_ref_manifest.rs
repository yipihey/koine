//! `schema-refs.json` names this crate as the writer of its kind, both ways.

#[test]
fn the_manifest_and_the_registration_agree() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../../schema-refs.json")).unwrap();
    let listed: Vec<&str> = manifest["registries"]["run-service"]
        .as_array()
        .expect("registries.run-service")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(listed, [run_service::kind::schema().name.as_str()]);
    for name in &listed {
        assert!(manifest["canonical"].get(*name).is_some(), "{name}");
    }
    assert_eq!(listed, [run_service::kind::RUN.as_str()]);
}
