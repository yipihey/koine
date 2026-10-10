//! Shared clap-from-inventory dispatch for `*-cli` binaries.
//!
//! Both `imbib-cli` and `imprint-cli` (and any future per-app CLI) iterate
//! [`crate::CliSubcommand`] and build a `clap::Command` tree at startup. The
//! schemas captured by the `#[impress_service]` macro carry enough type
//! information to derive `--flag` args automatically; this module centralizes
//! the conversion so the binaries stay tiny.
//!
//! ## Supported argument shapes
//!
//! The args struct emitted by the macro derives `JsonSchema`, producing a
//! JSON Schema object of the form:
//!
//! ```json
//! {
//!   "type": "object",
//!   "properties": {
//!     "input": { "type": "string" },
//!     "count": { "type": "integer" }
//!   },
//!   "required": ["input", "count"]
//! }
//! ```
//!
//! For Phase 3 we accept the primitive shapes used by the imbib/imprint
//! service traits: `string`, `integer`, `number`, `boolean`, `Option<String>`
//! (rendered as a nullable string), and `Vec<String>` (rendered as an array
//! of strings). Anything outside that set is currently exposed as a raw JSON
//! string flag (the user passes a JSON literal) — good enough for ship-ready
//! CLIs in this phase, and easy to extend later.
//!
//! Two ergonomics rules hold for every generated subcommand:
//!
//! - **Array flags are never CLI-required.** A non-Option `Vec<T>` field is
//!   required in the JSON Schema, but zero `--flag` occurrences parse fine
//!   and reach the handler as `[]` (dispatch inserts the empty array for
//!   schema-required properties). Handlers needing a non-empty list validate
//!   it themselves, exactly as they must for MCP callers.
//! - **Negative numbers work bare.** `--confidence -1` parses as a value on
//!   `integer`/`number` flags (and on arrays of them); the `--flag=-1` form
//!   is no longer necessary.
//!
//! ## Usage
//!
//! ```ignore
//! use impress_service_core::cli;
//!
//! fn main() {
//!     let app = cli::build_cli_from_inventory("imbib")
//!         .about("imbib service CLI — auto-generated from #[impress_service] traits");
//!     let matches = app.get_matches();
//!     match cli::dispatch_matches(&matches) {
//!         Ok(value) => {
//!             println!(
//!                 "{}",
//!                 serde_json::to_string_pretty(&value).unwrap_or_default()
//!             );
//!         }
//!         Err(e) => {
//!             eprintln!("error: {e}");
//!             std::process::exit(1);
//!         }
//!     }
//! }
//! ```

use std::collections::{HashMap, HashSet};

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value};

#[cfg(test)]
use crate::CliSubcommand;
use crate::{descriptor_handle::VerbHandle, BoxError};

/// Build a `clap::Command` for the binary named `app_name`, with one
/// subcommand per registered [`CliSubcommand`].
pub fn build_cli_from_inventory(app_name: &str) -> Command {
    let mut app = Command::new(app_name.to_string())
        .subcommand_required(true)
        .arg_required_else_help(true);

    for (name, sub) in effective_names() {
        app = app.subcommand(build_subcommand(name, sub));
    }

    app
}

/// Every linked subcommand with the name the CLI actually exposes it under.
///
/// The flat method name (`remove-tag`) wins whenever it is unique. When two
/// linked services declare the same method — `imbib-tags-service` and
/// `triage-service` both have `remove_tag` and `add_tag` — EVERY party to the
/// collision is exposed under its qualified `service_method` name instead,
/// so neither shadows the other and nothing that was unique changes
/// spelling. Before this, `impress-cli` panicked at startup on clap's
/// duplicate-name assertion the moment `impress-capabilities`' `full`
/// feature linked both services (2026-09-22), and no verb ran at all.
///
/// The choice is per BINARY, not per crate: a binary that links only one of
/// the two keeps the short name. That is deliberate — the short name is the
/// documented one and the prefix is the exception a collision earns.
pub fn effective_names() -> Vec<(String, VerbHandle)> {
    let verbs: Vec<_> = crate::call::descriptors().collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for verb in &verbs {
        *counts.entry(verb.method().to_owned()).or_default() += 1;
    }
    verbs
        .into_iter()
        .map(|verb| {
            let name = if counts[verb.method()] > 1 {
                verb.name()
            } else {
                verb.method()
            }
            .to_owned();
            (name, verb)
        })
        .collect()
}

/// Build one `clap::Command` from a single [`CliSubcommand`] descriptor,
/// under the name [`effective_names`] chose for it.
fn build_subcommand(name: String, sub: VerbHandle) -> Command {
    let schema = sub.input_schema();

    let mut cmd = Command::new(name).about(sub.description().to_string());

    let required: HashSet<String> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            cmd = cmd.arg(build_arg(name, prop, required.contains(name)));
        }
    }

    cmd
}

/// Build a `clap::Arg` from a single JSON-Schema property.
fn build_arg(name: &str, prop: &Value, required: bool) -> Arg {
    let kind = property_kind(prop);
    let long: String = name.replace('_', "-");

    let mut arg = Arg::new(name.to_string()).long(long).required(required);

    // An argument documented in `impress_service_impl!` (`/// …` before it)
    // carries a schema `description`; it is the flag's help, so `--help`
    // tells a CLI caller what the MCP `inputSchema` tells a model. `-h` shows
    // the first paragraph, `--help` all of it.
    if let Some(desc) = prop.get("description").and_then(|d| d.as_str()) {
        let short = desc.split("\n\n").next().unwrap_or(desc).to_string();
        arg = arg.help(short).long_help(desc.to_string());
    }

    match kind {
        PropertyKind::Boolean => {
            arg = arg.action(ArgAction::SetTrue).required(false);
        }
        PropertyKind::Array => {
            // Like `Boolean`, never CLI-required even when the schema says so:
            // a non-Option `Vec<T>` field is schema-required, but an empty
            // list is almost always a legitimate "unspecified" value, and
            // `matches_to_json` already folds zero occurrences of a
            // schema-required array to `[]` so the args struct deserializes.
            // Handlers that genuinely need >= 1 element validate that
            // themselves (and must — MCP callers can send `[]` too).
            arg = arg.action(ArgAction::Append).required(false);
            // `--nums -1` should be a value, not an unknown flag, when the
            // items are numeric (each repetition is parsed as a JSON literal).
            let items_are_numeric = prop
                .get("items")
                .map(|items| {
                    matches!(
                        property_kind(items),
                        PropertyKind::Integer | PropertyKind::Number
                    )
                })
                .unwrap_or(false);
            if items_are_numeric {
                arg = arg.allow_negative_numbers(true);
            }
        }
        PropertyKind::Integer | PropertyKind::Number => {
            // `--confidence -1` must parse as a value; without this clap
            // treats the bare `-1` token as an unknown flag and only the
            // `--confidence=-1` form works. Only tokens that look like
            // negative numbers are affected; `--confidence -abc` still errors.
            arg = arg.action(ArgAction::Set).allow_negative_numbers(true);
        }
        _ => {
            arg = arg.action(ArgAction::Set);
        }
    }

    arg
}

#[derive(Debug, Clone, Copy)]
enum PropertyKind {
    String,
    Integer,
    Number,
    Boolean,
    Array,
    /// Anything else (object, complex enum, untyped). The CLI accepts a raw
    /// JSON-literal string and we'll attempt to parse it during dispatch.
    Json,
}

fn property_kind(prop: &Value) -> PropertyKind {
    // Direct `"type": "string"` etc.
    if let Some(t) = prop.get("type") {
        if let Some(s) = t.as_str() {
            return match s {
                "string" => PropertyKind::String,
                "integer" => PropertyKind::Integer,
                "number" => PropertyKind::Number,
                "boolean" => PropertyKind::Boolean,
                "array" => PropertyKind::Array,
                _ => PropertyKind::Json,
            };
        }
        // `"type": ["string", "null"]` — Option<String>.
        if let Some(arr) = t.as_array() {
            for entry in arr {
                if entry.as_str() == Some("string") {
                    return PropertyKind::String;
                }
                if entry.as_str() == Some("integer") {
                    return PropertyKind::Integer;
                }
                if entry.as_str() == Some("number") {
                    return PropertyKind::Number;
                }
                if entry.as_str() == Some("boolean") {
                    return PropertyKind::Boolean;
                }
                if entry.as_str() == Some("array") {
                    return PropertyKind::Array;
                }
            }
        }
    }
    // schemars sometimes emits `"anyOf": [{"type": "string"}, {"type": "null"}]`
    if let Some(arr) = prop.get("anyOf").and_then(|v| v.as_array()) {
        for entry in arr {
            match property_kind(entry) {
                PropertyKind::Json => continue,
                other => return other,
            }
        }
    }
    PropertyKind::Json
}

/// Find the matched subcommand, build a JSON args object from the parsed
/// args, and run the handler on the shared runtime.
pub fn dispatch_matches(matches: &ArgMatches) -> Result<Value, BoxError> {
    let (sub_name, sub_matches) = matches
        .subcommand()
        .ok_or_else(|| -> BoxError { "no subcommand provided".into() })?;

    let descriptor = effective_names()
        .into_iter()
        .find(|(name, _)| name == sub_name)
        .map(|(_, sub)| sub)
        .ok_or_else(|| -> BoxError { format!("unknown subcommand `{sub_name}`").into() })?;

    let schema = descriptor.input_schema();
    let json_args = matches_to_json(sub_matches, &schema)?;

    // The CLI is an agent (ADR-0034 D3): whoever runs it, its verbs go
    // through the pipeline as `Agent("cli")`, never as the person.
    match crate::runtime::block_on(crate::pipeline::invoke_handle(
        descriptor,
        crate::pipeline::Call::agent("cli", json_args),
    )) {
        Ok(value) => Ok(value),
        Err(crate::pipeline::PipelineError::Handler(e)) => Err(e),
        Err(unavailable) => Err(unavailable.to_string().into()),
    }
}

fn matches_to_json(matches: &ArgMatches, schema: &Value) -> Result<Value, BoxError> {
    let mut map = Map::new();

    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let required: HashSet<String> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    for (name, prop) in &props {
        let kind = property_kind(prop);
        match kind {
            PropertyKind::Boolean => {
                let v = matches.get_flag(name);
                map.insert(name.clone(), Value::Bool(v));
            }
            PropertyKind::Array => {
                // An array of STRINGS takes its values verbatim; an array of
                // anything else (an array of objects — `Vec<SomeDto>`) takes
                // each repetition as a JSON literal, exactly as a scalar
                // `PropertyKind::Json` arg does.
                //
                // Without this branch such an argument is reachable over MCP
                // (which speaks real JSON) and unusable from the CLI, which
                // would hand serde a `Value::String` where the DTO belongs and
                // fail with `invalid type: string "...", expected struct X`.
                // Gated on the declared item type, so every existing
                // `Vec<String>` subcommand is bit-for-bit unaffected.
                let items_are_strings = prop
                    .get("items")
                    .map(|items| matches!(property_kind(items), PropertyKind::String))
                    .unwrap_or(true);
                let mut arr: Vec<Value> = Vec::new();
                if let Some(values) = matches.get_many::<String>(name) {
                    for v in values {
                        // `--tags '["a","b"]'`: one value that IS a JSON
                        // array of strings is taken as that array, the way
                        // every other structured argument takes JSON (review
                        // AC-F25: it was stored as the one literal tag
                        // `["a","b"]`). Repeating the flag still works.
                        let as_array = v
                            .trim_start()
                            .starts_with('[')
                            .then(|| serde_json::from_str::<Vec<String>>(v).ok())
                            .flatten();
                        if let (true, Some(items)) = (items_are_strings, as_array) {
                            arr.extend(items.into_iter().map(Value::String));
                        } else if items_are_strings {
                            arr.push(Value::String(v.clone()));
                        } else {
                            arr.push(serde_json::from_str(v).map_err(|e| -> BoxError {
                                format!("--{name}: expected a JSON literal per value ({e})").into()
                            })?);
                        }
                    }
                }
                if arr.is_empty() && !required.contains(name) {
                    // Optional + absent: leave null so serde Option<Vec<>>
                    // (if ever used) deserializes correctly.
                } else {
                    // NOTE: this branch is load-bearing for `build_arg`'s
                    // "arrays are never CLI-required" rule — a schema-required
                    // array with zero occurrences lands here and becomes `[]`,
                    // which is what lets the non-Option `Vec<T>` args struct
                    // deserialize.
                    map.insert(name.clone(), Value::Array(arr));
                }
            }
            PropertyKind::String => {
                if let Some(v) = matches.get_one::<String>(name) {
                    map.insert(name.clone(), Value::String(v.clone()));
                } else if !required.contains(name) {
                    map.insert(name.clone(), Value::Null);
                }
            }
            PropertyKind::Integer => {
                if let Some(v) = matches.get_one::<String>(name) {
                    let parsed: i64 = v.parse().map_err(|e| -> BoxError {
                        format!("--{name}: expected integer ({e})").into()
                    })?;
                    map.insert(name.clone(), Value::from(parsed));
                } else if !required.contains(name) {
                    map.insert(name.clone(), Value::Null);
                }
            }
            PropertyKind::Number => {
                if let Some(v) = matches.get_one::<String>(name) {
                    let parsed: f64 = v.parse().map_err(|e| -> BoxError {
                        format!("--{name}: expected number ({e})").into()
                    })?;
                    map.insert(
                        name.clone(),
                        serde_json::Number::from_f64(parsed)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                    );
                } else if !required.contains(name) {
                    map.insert(name.clone(), Value::Null);
                }
            }
            PropertyKind::Json => {
                if let Some(v) = matches.get_one::<String>(name) {
                    let parsed: Value = serde_json::from_str(v).map_err(|e| -> BoxError {
                        format!("--{name}: expected JSON literal ({e})").into()
                    })?;
                    map.insert(name.clone(), parsed);
                } else if !required.contains(name) {
                    map.insert(name.clone(), Value::Null);
                }
            }
        }
    }

    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `Command` with one `--<name>` argument built from `schema` (whose
    /// `required` array decides required-ness, exactly like
    /// `build_subcommand`), matched against `argv`, then folded back to the
    /// JSON the handler sees.
    fn round_trip_arg(name: &str, schema: Value, argv: &[&str]) -> Result<Value, String> {
        let prop = schema["properties"][name].clone();
        let required = schema["required"]
            .as_array()
            .map(|arr| arr.iter().any(|v| v.as_str() == Some(name)))
            .unwrap_or(false);
        let cmd = Command::new("t")
            .no_binary_name(true)
            .arg(build_arg(name, &prop, required));
        let matches = cmd.try_get_matches_from(argv).map_err(|e| e.to_string())?;
        matches_to_json(&matches, &schema).map_err(|e| e.to_string())
    }

    /// A documented argument's schema `description` is its `--help` text:
    /// the first paragraph for `-h`, the whole of it for `--help`.
    #[test]
    fn a_described_argument_carries_its_description_as_help() {
        let prop = json!({
            "description": "Inline data.\n\nExample: --series '{\"x\":[1],\"y\":[2]}'",
            "type": ["string", "null"],
        });
        let arg = build_arg("series", &prop, false);
        assert_eq!(
            arg.get_help().map(|h| h.to_string()),
            Some("Inline data.".into())
        );
        assert!(arg
            .get_long_help()
            .map(|h| h.to_string())
            .unwrap_or_default()
            .contains("Example: --series"));
        let bare = build_arg("x", &json!({"type": "string"}), false);
        assert!(bare.get_help().is_none());
    }

    /// A `Command` with one repeatable `--items` argument built from `schema`,
    /// matched against `argv`, then folded back to the JSON the handler sees.
    fn round_trip(schema: Value, argv: &[&str]) -> Result<Value, String> {
        round_trip_arg("items", schema, argv)
    }

    fn array_schema(item_type: &str) -> Value {
        json!({
            "type": "object",
            "properties": {
                "items": { "type": "array", "items": { "type": item_type } }
            }
        })
    }

    /// The shipped shape — `Vec<String>` args take their values verbatim. This
    /// is the regression guard on the object-array branch: every existing
    /// subcommand (`add_members`, `member_counts`, `record_produced_rows`)
    /// passes ids as bare strings and must keep doing so.
    #[test]
    fn a_string_array_takes_its_values_verbatim() {
        let out = round_trip(
            array_schema("string"),
            &["--items", "alpha", "--items", "{not json"],
        )
        .expect("string arrays never parse their values");
        assert_eq!(out["items"], json!(["alpha", "{not json"]));
    }

    /// AC-F25: one value that is a JSON array of strings is that array; a
    /// value that merely starts with `[` but is not one stays verbatim.
    #[test]
    fn a_string_array_accepts_one_json_array() {
        let out = round_trip(array_schema("string"), &["--items", r#"["a", "b"]"#])
            .expect("a JSON array of strings");
        assert_eq!(out["items"], json!(["a", "b"]));
        let out = round_trip(array_schema("string"), &["--items", "[draft"]).unwrap();
        assert_eq!(out["items"], json!(["[draft"]));
    }

    /// `Vec<SomeDto>`: each repetition is a JSON literal. Without this the
    /// argument is reachable over MCP and unusable from the CLI, which fails
    /// with `invalid type: string "...", expected struct X`.
    #[test]
    fn an_object_array_parses_each_value_as_json() {
        let out = round_trip(
            array_schema("object"),
            &[
                "--items",
                r#"{"path":"/a.bib"}"#,
                "--items",
                r#"{"path":"/b.ris"}"#,
            ],
        )
        .expect("object arrays parse per value");
        assert_eq!(
            out["items"],
            json!([{"path": "/a.bib"}, {"path": "/b.ris"}])
        );
    }

    #[test]
    fn an_object_array_reports_bad_json_against_the_argument() {
        let err = round_trip(array_schema("object"), &["--items", "not json"])
            .expect_err("malformed JSON must fail loudly");
        assert!(err.contains("--items"), "{err}");
        assert!(err.contains("JSON literal"), "{err}");
    }

    /// An absent optional array stays absent rather than becoming `[]` —
    /// unchanged by this branch, and the difference matters to a handler that
    /// distinguishes "not supplied" from "supplied empty".
    #[test]
    fn an_absent_optional_array_is_omitted() {
        let out = round_trip(array_schema("object"), &[]).expect("absent is fine");
        assert!(out.get("items").is_none());
    }

    fn required_array_schema(item_type: &str) -> Value {
        json!({
            "type": "object",
            "properties": {
                "items": { "type": "array", "items": { "type": item_type } }
            },
            "required": ["items"]
        })
    }

    /// The empty-`Vec<String>` fix: a non-Option `Vec<String>` field is
    /// schema-required, but zero `--items` occurrences must parse (no more
    /// `--items ""` idiom) and reach the handler as `[]` so the args struct
    /// deserializes.
    #[test]
    fn a_schema_required_array_parses_with_zero_occurrences_as_empty() {
        let out = round_trip(required_array_schema("string"), &[])
            .expect("required arrays are CLI-optional");
        assert_eq!(out["items"], json!([]));
    }

    /// ...and still takes its values verbatim when supplied.
    #[test]
    fn a_schema_required_array_still_accepts_values() {
        let out = round_trip(required_array_schema("string"), &["--items", "alpha"])
            .expect("supplying values still works");
        assert_eq!(out["items"], json!(["alpha"]));
    }

    /// Required-ness is only relaxed for arrays (and booleans, historically):
    /// a required scalar still errors when absent, so genuinely mandatory
    /// string/number args keep their guardrail.
    #[test]
    fn a_schema_required_string_is_still_cli_required() {
        let schema = json!({
            "type": "object",
            "properties": { "input": { "type": "string" } },
            "required": ["input"]
        });
        let err = round_trip_arg("input", schema, &[]).expect_err("required string must error");
        assert!(err.contains("--input"), "{err}");
    }

    /// The negative-number fix: `--confidence -1` is a value, not an unknown
    /// flag, on number args — the `--confidence=-1` workaround is no longer
    /// needed.
    #[test]
    fn a_number_flag_accepts_a_bare_negative_value() {
        let schema = json!({
            "type": "object",
            "properties": { "confidence": { "type": "number" } },
            "required": ["confidence"]
        });
        let out = round_trip_arg("confidence", schema, &["--confidence", "-1"])
            .expect("bare negative number parses");
        assert_eq!(out["confidence"], json!(-1.0));
    }

    #[test]
    fn an_integer_flag_accepts_a_bare_negative_value() {
        let schema = json!({
            "type": "object",
            "properties": { "offset": { "type": "integer" } }
        });
        let out = round_trip_arg("offset", schema, &["--offset", "-3"])
            .expect("bare negative integer parses");
        assert_eq!(out["offset"], json!(-3));
    }

    /// Non-numeric garbage after a number flag still fails loudly — the
    /// negative-number allowance only admits tokens that look like numbers.
    #[test]
    fn a_number_flag_still_rejects_a_dash_word() {
        let schema = json!({
            "type": "object",
            "properties": { "confidence": { "type": "number" } }
        });
        let err = round_trip_arg("confidence", schema, &["--confidence", "-abc"])
            .expect_err("a dash-word is not a number value");
        assert!(err.contains("-abc") || err.contains("unexpected"), "{err}");
    }

    /// Arrays of numbers get the same allowance, through the JSON-literal
    /// per-value path.
    #[test]
    fn an_integer_array_accepts_bare_negative_values() {
        let out = round_trip(
            required_array_schema("integer"),
            &["--items", "-1", "--items", "-2"],
        )
        .expect("negative array items parse");
        assert_eq!(out["items"], json!([-1, -2]));
    }
}

#[cfg(test)]
mod collision_tests {
    use super::*;

    fn schema() -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn apply(_: serde_json::Value) -> crate::ServiceFuture {
        Box::pin(async { Ok(serde_json::Value::Null) })
    }

    const fn probe(
        service: &'static str,
        method: &'static str,
        name: &'static str,
    ) -> crate::VerbDescriptor {
        crate::VerbDescriptor {
            name,
            service,
            method,
            description: "",
            input_schema: schema,
            output_schema: schema,
            safety: crate::Safety {
                class: crate::SafetyClass::ReadOnly,
                idempotent: true,
            },
            since: "0.1.0",
            effects: crate::Effects::NONE,
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            source: crate::Source::Linked,
            handler: apply,
        }
    }
    static ALPHA_COLLIDE: crate::VerbDescriptor = probe(
        "alpha-service",
        "probe-collide",
        "alpha-service_probe-collide",
    );
    static BETA_COLLIDE: crate::VerbDescriptor = probe(
        "beta-service",
        "probe-collide",
        "beta-service_probe-collide",
    );
    static ALPHA_UNIQUE: crate::VerbDescriptor = probe(
        "alpha-service",
        "probe-unique",
        "alpha-service_probe-unique",
    );

    // Two services that both declare `probe_collide`, and one that alone
    // declares `probe_unique` — submitted into the same process-wide
    // inventory the real services use, so this pins the rule on the real
    // path rather than a model of it.
    inventory::submit! { CliSubcommand::of(&ALPHA_COLLIDE) }
    inventory::submit! { crate::McpToolDescriptor::of(&ALPHA_COLLIDE) }
    inventory::submit! { CliSubcommand::of(&BETA_COLLIDE) }
    inventory::submit! { crate::McpToolDescriptor::of(&BETA_COLLIDE) }
    inventory::submit! { CliSubcommand::of(&ALPHA_UNIQUE) }
    inventory::submit! { crate::McpToolDescriptor::of(&ALPHA_UNIQUE) }

    /// A unique method keeps its short name; every party to a collision is
    /// exposed qualified, and the bare colliding name is exposed by nobody.
    #[test]
    fn a_collision_qualifies_every_party_and_leaves_unique_names_alone() {
        let names: Vec<String> = effective_names().into_iter().map(|(n, _)| n).collect();
        assert!(names.iter().any(|n| n == "probe-unique"));
        assert!(names.iter().any(|n| n == "alpha-service_probe-collide"));
        assert!(names.iter().any(|n| n == "beta-service_probe-collide"));
        assert!(
            !names.iter().any(|n| n == "probe-collide"),
            "a shadowed verb is worse than a renamed one"
        );
    }

    /// The failure this exists to prevent: clap asserts unique subcommand
    /// names at build time, and used to panic in `impress-cli`'s main.
    #[test]
    fn the_built_command_has_no_duplicate_subcommands() {
        let app = build_cli_from_inventory("probe");
        let mut seen = std::collections::HashSet::new();
        for sub in app.get_subcommands() {
            assert!(
                seen.insert(sub.get_name().to_string()),
                "duplicate: {}",
                sub.get_name()
            );
        }
        app.debug_assert();
    }
}
