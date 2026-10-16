//! Schema lowering over derived and linked schemas (LM-D3): every profile
//! lowers or refuses with a path, the Apple output keeps its invariants, and
//! (on a Mac, `KOINE_TEST_APPLE_FM=1`) Foundation Models decodes every
//! Apple-profile schema into a `GenerationSchema`.

use koine_ai::schema::{lower, Profile};
use serde_json::{json, Value};

const PROFILES: [Profile; 3] = [
    Profile::AppleGuided,
    Profile::LlamaGrammar,
    Profile::OpenAiStrict,
];

/// Hand-written schemas as `schemars` 0.8 writes them.
fn fixtures() -> Vec<(&'static str, Value)> {
    vec![
        (
            "weather",
            json!({
                "type": "object", "required": ["city"],
                "properties": {
                    "city": { "type": "string" },
                    "unit": { "type": "string", "enum": ["c", "f"] }
                }
            }),
        ),
        (
            "nested_ref",
            json!({
                "type": "object", "required": ["target"],
                "properties": { "target": { "description": "The target",
                    "allOf": [{ "$ref": "#/definitions/Target" }] } },
                "definitions": { "Target": { "type": "object", "required": ["id"],
                    "properties": { "id": { "type": "string", "format": "uuid" },
                                    "note": { "type": ["string", "null"] } } } }
            }),
        ),
        (
            "unit_enum_and_option",
            json!({
                "type": "object", "required": ["unit"],
                "properties": {
                    "unit": { "oneOf": [{ "type": "string", "enum": ["a"] }, { "const": "b" }] },
                    "maybe": { "anyOf": [{ "type": "integer", "minimum": 1.0, "maximum": 9.0 },
                                         { "type": "null" }] }
                }
            }),
        ),
        (
            "tagged_enum",
            json!({
                "type": "object", "required": ["q"],
                "properties": { "q": { "oneOf": [
                    { "type": "object", "required": ["kind", "text"], "properties": {
                        "kind": { "type": "string", "enum": ["text"] },
                        "text": { "type": "string" } } },
                    { "type": "object", "required": ["kind"], "properties": {
                        "kind": { "type": "string", "enum": ["all"] },
                        "limit": { "type": "integer" } } }
                ] } }
            }),
        ),
        (
            "array_of_objects",
            json!({
                "type": "object", "required": ["filters"],
                "properties": { "filters": { "type": "array", "minItems": 1,
                    "items": { "type": "object", "required": ["f"],
                               "properties": { "f": { "type": "string" } } } },
                    "tags": { "type": "array", "items": { "type": "string" } } }
            }),
        ),
        ("parameterless", json!({ "type": "object" })),
    ]
}

/// Every object node carries the four keys; no reference remains.
fn assert_apple_invariants(v: &Value, at: &str) {
    match v {
        Value::Object(m) => {
            assert!(!m.contains_key("$ref"), "$ref at {at}: {v}");
            assert!(
                !m.contains_key("definitions") && !m.contains_key("$defs"),
                "{at}"
            );
            if m.get("type") == Some(&json!("object")) {
                for k in ["title", "required", "x-order", "additionalProperties"] {
                    assert!(m.contains_key(k), "{k} missing at {at}: {v}");
                }
                assert_eq!(m["additionalProperties"], json!(false), "{at}");
                let props = m["properties"].as_object().expect("properties");
                let order: Vec<&str> = m["x-order"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap())
                    .collect();
                let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
                let mut sorted_order = order.clone();
                keys.sort_unstable();
                sorted_order.sort_unstable();
                assert_eq!(keys, sorted_order, "x-order names every property at {at}");
                for r in m["required"].as_array().unwrap() {
                    assert!(props.contains_key(r.as_str().unwrap()), "{at}");
                }
            }
            for (k, child) in m {
                assert_apple_invariants(child, &format!("{at}.{k}"));
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                assert_apple_invariants(child, &format!("{at}.{i}"));
            }
        }
        _ => {}
    }
}

#[test]
fn fixtures_lower_for_every_profile() {
    for (name, schema) in fixtures() {
        for p in PROFILES {
            let out = lower(&schema, "Args", p).unwrap_or_else(|e| panic!("{name} {p:?}: {e}"));
            if p == Profile::AppleGuided {
                assert_apple_invariants(&out, name);
            }
        }
    }
}

#[cfg(feature = "schema")]
mod derived {
    use super::*;
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Filter {
        field: String,
        value: Option<String>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    enum Unit {
        Celsius,
        Fahrenheit,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Query {
        Text { text: String },
        Recent { limit: Option<u32> },
    }

    /// Mirrors a verb's argument struct.
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Args {
        /// The conversation.
        conversation: uuid::Uuid,
        note: Option<String>,
        filters: Vec<Filter>,
        unit: Unit,
        maybe_unit: Option<Unit>,
        query: Query,
        count: Option<u32>,
        flag: bool,
        ratio: f64,
    }

    #[derive(JsonSchema)]
    struct Nothing {}

    fn schema_of<T: JsonSchema>() -> Value {
        serde_json::to_value(schemars::schema_for!(T)).unwrap()
    }

    pub fn derived() -> Vec<(&'static str, Value)> {
        vec![
            ("derived_args", schema_of::<Args>()),
            ("derived_nothing", schema_of::<Nothing>()),
        ]
    }

    #[test]
    fn derived_schemas_lower_for_every_profile() {
        for (name, schema) in derived() {
            for p in PROFILES {
                let out = lower(&schema, "Args", p).unwrap_or_else(|e| panic!("{name} {p:?}: {e}"));
                let text = out.to_string();
                assert!(!text.contains("\"$ref\""), "{name} {p:?}: {text}");
                if p == Profile::AppleGuided {
                    assert_apple_invariants(&out, name);
                }
            }
        }
        let apple = lower(&schema_of::<Args>(), "Args", Profile::AppleGuided).unwrap();
        let required = apple["required"].as_array().unwrap();
        assert!(required.contains(&json!("conversation")));
        assert!(!required.contains(&json!("note")));
        assert_eq!(
            apple["properties"]["conversation"]["description"],
            "The conversation."
        );
    }
}

#[cfg(feature = "service")]
mod inventory {
    use super::*;
    use impress_service_core::call;

    pub fn lowered_inventory() -> (Vec<(String, Value)>, String) {
        koine_agent::ensure_linked();
        koine_ai::ensure_linked();
        let mut apple_ok = Vec::new();
        let mut report = String::new();
        for p in PROFILES {
            let (mut ok, mut bad) = (0, Vec::new());
            for handle in call::descriptors() {
                let schema = handle.input_schema();
                match lower(&schema, handle.name(), p) {
                    Ok(out) => {
                        ok += 1;
                        if p == Profile::AppleGuided {
                            assert_apple_invariants(&out, handle.name());
                            apple_ok.push((handle.name().to_string(), out));
                        }
                    }
                    Err(e) => {
                        assert!(
                            !e.path.is_empty() || !e.reason.is_empty(),
                            "{}: empty Unsupported",
                            handle.name()
                        );
                        bad.push(format!("  {} at `{}`: {}", handle.name(), e.path, e.reason));
                    }
                }
            }
            report.push_str(&format!("{p:?}: ok {ok}, unsupported {}\n", bad.len()));
            for line in bad {
                report.push_str(&line);
                report.push('\n');
            }
        }
        (apple_ok, report)
    }

    #[test]
    fn every_linked_verb_lowers_or_says_why_not() {
        let (_, report) = lowered_inventory();
        println!("{report}");
    }
}

#[cfg(target_os = "macos")]
mod apple {
    use super::*;
    use std::process::Command;

    const SWIFT: &str = r#"
import Foundation
import FoundationModels

@main
struct Decode {
    static func main() {
        var failed = false
        for path in CommandLine.arguments.dropFirst() {
            do {
                let data = try Data(contentsOf: URL(fileURLWithPath: path))
                _ = try JSONDecoder().decode(GenerationSchema.self, from: data)
                print("ok \(path)")
            } catch {
                failed = true
                print("fail \(path): \(error)")
            }
        }
        exit(failed ? 1 : 0)
    }
}
"#;

    #[test]
    fn every_apple_profile_schema_decodes_into_a_generation_schema() {
        if std::env::var("KOINE_TEST_APPLE_FM").as_deref() != Ok("1") {
            eprintln!("skipped: set KOINE_TEST_APPLE_FM=1 on a Mac with Apple Intelligence");
            return;
        }
        let mut outputs: Vec<(String, Value)> = Vec::new();
        for (name, schema) in fixtures() {
            outputs.push((
                name.to_string(),
                lower(&schema, "Args", Profile::AppleGuided).unwrap(),
            ));
        }
        #[cfg(feature = "schema")]
        for (name, schema) in derived::derived() {
            outputs.push((
                name.to_string(),
                lower(&schema, "Args", Profile::AppleGuided).unwrap(),
            ));
        }
        #[cfg(feature = "service")]
        outputs.extend(inventory::lowered_inventory().0);

        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for (name, out) in &outputs {
            let file = dir.path().join(format!("{name}.json"));
            std::fs::write(&file, serde_json::to_vec_pretty(out).unwrap()).unwrap();
            files.push(file);
        }
        let source = dir.path().join("decode.swift");
        std::fs::write(&source, SWIFT).unwrap();
        let binary = dir.path().join("decode");
        let build = Command::new("swiftc")
            .args(["-parse-as-library", "-o"])
            .arg(&binary)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "swiftc: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let run = Command::new(&binary).args(&files).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        println!("{stdout}");
        assert!(
            run.status.success(),
            "some schemas did not decode:\n{stdout}"
        );
        assert_eq!(
            stdout.matches("\nok ").count() + usize::from(stdout.starts_with("ok ")),
            files.len()
        );
    }
}
