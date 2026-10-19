use run_convention::{check_dir, check_parameter_schema, schemas};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn rules(name: &str) -> Vec<&'static str> {
    let mut r: Vec<&'static str> = check_dir(fixture(name))
        .findings
        .iter()
        .map(|f| f.rule)
        .collect();
    r.dedup();
    r
}

fn copy_good() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    copy(&fixture("good"), t.path());
    t
}

fn copy(from: &Path, to: &Path) {
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

#[test]
fn good_conforms() {
    let r = check_dir(fixture("good"));
    assert!(r.ok(), "{r}");
}

#[test]
fn gapped_seq_is_reported() {
    assert_eq!(rules("gapped-seq"), ["seq"]);
    let r = check_dir(fixture("gapped-seq"));
    assert_eq!(r.findings[0].file, "events.jsonl");
}

#[test]
fn bad_key_is_reported() {
    assert_eq!(rules("bad-key"), ["key"]);
}

#[test]
fn missing_output_is_reported() {
    assert_eq!(rules("missing-output"), ["output-missing"]);
}

#[test]
fn bad_exit_band_is_reported_in_both_files() {
    let r = check_dir(fixture("bad-exit-band"));
    assert_eq!(r.with_rule("exit-band").len(), 2, "{r}");
    assert!(r.findings.iter().all(|f| f.rule == "exit-band"), "{r}");
}

#[test]
fn trailing_partial_line_is_ignored_but_a_bad_middle_line_is_not() {
    let t = copy_good();
    let p = t.path().join("events.jsonl");
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str("{\"seq\": 9, \"t\": \"2026-10-19T14:10:00Z\", \"ki");
    fs::write(&p, &s).unwrap();
    assert!(check_dir(t.path()).ok());

    let s = s.replacen("\"kind\": \"warning\"", "\"kind\": \"warning\" oops", 1);
    fs::write(&p, s).unwrap();
    assert!(!check_dir(t.path()).with_rule("parse").is_empty());
}

#[test]
fn per_kind_required_fields_are_enforced() {
    let t = copy_good();
    let p = t.path().join("events.jsonl");
    let s = fs::read_to_string(&p).unwrap().replace(
        ", \"step\": 100, \"time\": 3.1e5, \"dt\": 12.0, \"fraction\": 0.03",
        ", \"fraction\": 3",
    );
    fs::write(&p, s).unwrap();
    let r = check_dir(t.path());
    assert!(r.with_rule("schema").len() >= 2, "{r}");
}

#[test]
fn unknown_kind_and_bad_params_are_schema_errors() {
    let t = copy_good();
    let p = t.path().join("events.jsonl");
    let s = fs::read_to_string(&p)
        .unwrap()
        .replace("\"started\"", "\"begun\"");
    fs::write(&p, s).unwrap();
    let q = t.path().join("params.json");
    fs::write(
        &q,
        r#"{"flat": {"value": 1, "source": "default"}, "a.b": {"value": 1}}"#,
    )
    .unwrap();
    let r = check_dir(t.path());
    assert!(
        r.findings
            .iter()
            .any(|f| f.file == "events.jsonl" && f.rule == "schema"),
        "{r}"
    );
    assert!(
        r.findings
            .iter()
            .any(|f| f.file == "params.json" && f.rule == "schema"),
        "{r}"
    );
}

#[test]
fn output_event_must_be_indexed_and_paths_stay_inside() {
    let t = copy_good();
    fs::write(
        t.path().join("outputs.json"),
        r#"{"outputs": [{"path": "../x", "kind": "other"}]}"#,
    )
    .unwrap();
    let r = check_dir(t.path());
    assert!(!r.with_rule("path").is_empty(), "{r}");
    assert!(!r.with_rule("output-index").is_empty(), "{r}");
}

#[test]
fn refused_control_needs_a_reason() {
    let t = copy_good();
    fs::write(
        t.path().join("control/refused/9c1b.json"),
        r#"{"request": "set", "param": "grid.ncell", "value": 800}"#,
    )
    .unwrap();
    fs::write(
        t.path().join("control/pending.json"),
        r#"{"request": "dance"}"#,
    )
    .unwrap();
    let r = check_dir(t.path());
    assert_eq!(r.with_rule("control").len(), 1, "{r}");
    assert!(
        r.findings
            .iter()
            .any(|f| f.file == "control/pending.json" && f.rule == "schema"),
        "{r}"
    );
}

#[test]
fn missing_and_empty_dirs() {
    let t = tempfile::tempdir().unwrap();
    let r = check_dir(t.path());
    assert_eq!(r.with_rule("missing-file").len(), 2);
    assert!(!check_dir(t.path().join("nope")).ok());
}

#[test]
fn imported_run_may_omit_key_native_may_not() {
    let t = copy_good();
    let p = t.path().join("run.json");
    let mut v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("key");
    fs::write(&p, v.to_string()).unwrap();
    assert!(!check_dir(t.path()).ok());
    v["provenance"] = json!("imported");
    fs::write(&p, v.to_string()).unwrap();
    assert!(check_dir(t.path()).ok());
}

#[test]
fn every_bundled_schema_is_a_valid_schema() {
    for (name, text) in [
        ("run", schemas::RUN),
        ("params", schemas::PARAMS),
        ("event", schemas::EVENT),
        ("outputs", schemas::OUTPUTS),
        ("sweep", schemas::SWEEP),
        ("control", schemas::CONTROL),
        ("parameter-schema", schemas::PARAMETER_SCHEMA),
    ] {
        let v: Value = serde_json::from_str(text).unwrap();
        jsonschema::meta::validate(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(v["$schema"], "https://json-schema.org/draft/2020-12/schema");
    }
}

#[test]
fn sweep_schema() {
    let v: Value = serde_json::from_str(schemas::SWEEP).unwrap();
    let val = jsonschema::validator_for(&v).unwrap();
    let ok = json!({
        "id": "5b0f6a3e-2c1d-4e7a-9f55-0a1b2c3d4e5f",
        "base": {"grid.ncell": 400},
        "vary": [{"param": "sink.tau", "values": [1.0, 2.0]},
                 {"param": "seed.n", "generator": {"kind": "sampler", "seed": 7}}],
        "runs": ["6b0f6a3e-2c1d-4e7a-9f55-0a1b2c3d4e5f"]
    });
    assert!(val.is_valid(&ok));
    let mut no_seed = ok.clone();
    no_seed["vary"][1]["generator"] = json!({"kind": "sampler"});
    assert!(!val.is_valid(&no_seed));
    let mut both = ok.clone();
    both["vary"][0]["generator"] = json!({"kind": "grid"});
    assert!(!val.is_valid(&both));
}

#[test]
fn parameter_meta_schema() {
    let good = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "sink.tau": {
                "type": "number", "default": 0.0, "minimum": 0,
                "description": "inner sink relaxation time; 0 = closed wall",
                "x-run": {"unit": "s", "group": "sink", "mutable": "never",
                          "env": "QS_SINK_TAU", "requires": [{"param": "sink.gasrow", "equals": 1}]}
            }
        },
        "x-run": {"metrics": [{"name": "mach", "unit": "", "description": "shock Mach number"}],
                  "groups": {"commons": {"open": true, "generator": "TitanFortran.parse_commons"}}}
    });
    assert_eq!(check_parameter_schema(&good), Vec::<String>::new());

    let mut bad = good.clone();
    bad["properties"]["sink.tau"]["x-run"]["mutable"] = json!("sometimes");
    assert!(!check_parameter_schema(&bad).is_empty());
    let mut bad = good.clone();
    bad["properties"]["sink.tau"]["x-run"]["mutible"] = json!("never");
    assert!(!check_parameter_schema(&bad).is_empty());
    let mut bad = good.clone();
    bad["x-run"]["groups"]["commons"] = json!({"open": true});
    assert!(!check_parameter_schema(&bad).is_empty());
    let mut bad = good.clone();
    bad["properties"]["tau"] = bad["properties"]["sink.tau"].clone();
    assert!(!check_parameter_schema(&bad).is_empty());
    let mut bad = good;
    bad["additionalProperties"] = json!(true);
    assert!(!check_parameter_schema(&bad).is_empty());
}
