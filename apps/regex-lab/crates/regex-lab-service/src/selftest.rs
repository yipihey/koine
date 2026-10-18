//! Tier A: the verbs, called the way MCP, the CLI and the host call them.
//!
//! Every call goes through `impress_service_core::call` - find the verb in
//! the linked inventory, run it through the pipeline - never through the
//! trait directly, which would not catch a macro-wiring mistake, a missing
//! example or a strict-argument refusal.
//!
//! The process gets a memory store once, as a host would install its own, so
//! the saved-pattern verbs have somewhere to write.

use std::sync::{Arc, Once};

use impress_service_core::{call, VerbHandle};
use serde_json::{json, Value};

const SERVICE: &str = "regex-lab-service";

fn with_store() {
    static STORE: Once = Once::new();
    STORE.call_once(|| {
        koine_store::process::install(Arc::new(koine_store_memory::MemoryStore::new()));
    });
}

fn verbs() -> Vec<VerbHandle> {
    let mut verbs: Vec<VerbHandle> = call::descriptors()
        .filter(|v| v.service() == SERVICE)
        .collect();
    verbs.sort_by(|a, b| a.name().cmp(b.name()));
    verbs
}

fn run(verb: &str, args: Value) -> Value {
    with_store();
    call::call(&format!("{SERVICE}_{verb}"), args).unwrap()
}

/// Every key of `expected` is in `actual` with the same value (objects
/// recurse; anything else must be equal).
fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e
            .iter()
            .all(|(key, want)| a.get(key).is_some_and(|got| contains(got, want))),
        _ => actual == expected,
    }
}

#[test]
fn the_service_is_linked_and_every_verb_is_documented() {
    let verbs = verbs();
    assert_eq!(
        verbs.len(),
        14,
        "{:?}",
        verbs.iter().map(|v| v.name()).collect::<Vec<_>>()
    );
    for verb in verbs {
        assert!(
            !verb.description().is_empty(),
            "{} has no description",
            verb.name()
        );
        assert!(
            !verb.examples().is_empty(),
            "{} has no #[impress_example]: Tier A runs a verb only through its examples",
            verb.name()
        );
    }
}

/// Run every example of every verb and check its `expect`. This is the test
/// that grows with the app: a verb with an example is a verb under test.
#[test]
fn every_example_runs() {
    with_store();
    for verb in verbs() {
        for example in verb.examples() {
            let label = format!("{} / {}", verb.name(), example.name);
            let result = call::call(verb.name(), example.args_value())
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_ne!(result["ok"], json!(false), "{label} was refused: {result}");
            if let Some(expect) = example.expect {
                let expect: Value = serde_json::from_str(expect).expect("expect is JSON");
                assert!(
                    contains(&result, &expect),
                    "{label}: {result} does not contain {expect}"
                );
            }
        }
    }
}

#[test]
fn a_pattern_that_does_not_parse_is_refused_with_its_place() {
    let result = run("check", json!({ "pattern": "a(b|c" }));
    assert_eq!(result["ok"], json!(false), "{result}");
    assert_eq!(result["code"], json!("invalid-pattern"));
    assert_eq!(result["at"]["column"], json!(2), "{result}");
    assert!(result["diagram"].as_str().unwrap().contains("a(b|c"));
    // Every matching verb refuses the same way, before it reads the text.
    for verb in [
        "is-match",
        "find",
        "find-all",
        "captures",
        "captures-all",
        "split",
    ] {
        let result = run(verb, json!({ "pattern": "[z-a]", "haystack": "x" }));
        assert_eq!(result["code"], json!("invalid-pattern"), "{verb}: {result}");
        assert_eq!(result["at"]["start"], json!(1), "{verb}: {result}");
    }
}

#[test]
fn a_pattern_past_the_size_limit_is_refused_as_too_big() {
    let result = run("check", json!({ "pattern": r"\w{1000}{1000}" }));
    assert_eq!(result["code"], json!("pattern-too-big"), "{result}");
}

#[test]
fn pattern_and_saved_are_one_or_the_other() {
    let neither = run("find", json!({ "haystack": "x" }));
    assert_eq!(neither["code"], json!("invalid-argument"), "{neither}");
    let both = run(
        "find",
        json!({ "pattern": "x", "saved": "x", "haystack": "x" }),
    );
    assert_eq!(both["code"], json!("invalid-argument"), "{both}");
    let missing = run(
        "find",
        json!({ "saved": "not-saved-anywhere", "haystack": "x" }),
    );
    assert_eq!(missing["code"], json!("not-found"), "{missing}");
    // The empty pattern is a regex: it matches at every position.
    let empty = run("find-all", json!({ "pattern": "", "haystack": "ab" }));
    assert_eq!(empty["count"], json!(3), "{empty}");
    // An empty `saved` names nothing, so the pattern is used...
    let unnamed = run(
        "find",
        json!({ "pattern": "b", "saved": "", "haystack": "ab" }),
    );
    assert_eq!(unnamed["text"], json!("b"), "{unnamed}");
    // ...and an empty pattern beside a name means the name.
    let named = run(
        "find",
        json!({ "pattern": "", "saved": "not-saved-anywhere", "haystack": "x" }),
    );
    assert_eq!(named["code"], json!("not-found"), "{named}");
}

#[test]
fn flags_change_the_match_and_a_bad_letter_is_refused() {
    let plain = run("is-match", json!({ "pattern": "^b", "haystack": "a\nb" }));
    assert_eq!(plain["matched"], json!(false));
    let multi = run(
        "is-match",
        json!({ "pattern": "^b", "haystack": "a\nb", "flags": "m" }),
    );
    assert_eq!(multi["matched"], json!(true));
    let bad = run(
        "is-match",
        json!({ "pattern": "a", "haystack": "a", "flags": "g" }),
    );
    assert_eq!(bad["code"], json!("invalid-argument"), "{bad}");
}

#[test]
fn a_saved_pattern_is_used_by_name_with_its_flags() {
    let saved = run(
        "save-pattern",
        json!({ "name": "shout", "pattern": "hello", "flags": "i", "sample": "HELLO" }),
    );
    assert_eq!(saved["ok"], json!(true), "{saved}");
    let found = run("find", json!({ "saved": "shout", "haystack": "say Hello" }));
    assert_eq!(found["text"], json!("Hello"), "{found}");
    // Flags given replace the saved ones.
    let strict = run(
        "find",
        json!({ "saved": "shout", "flags": "", "haystack": "say Hello" }),
    );
    assert_eq!(strict["found"], json!(false), "{strict}");
    // Saving again replaces it.
    let again = run("save-pattern", json!({ "name": "shout", "pattern": "hi" }));
    assert_eq!(again["created"], json!(false), "{again}");
    let listed = run("patterns", json!({ "search": "shout" }));
    assert_eq!(listed[0]["pattern"], json!("hi"), "{listed}");
    // Deleting twice is deleting once.
    assert_eq!(
        run("delete-pattern", json!({ "name": "shout" }))["deleted"],
        json!(true)
    );
    assert_eq!(
        run("delete-pattern", json!({ "name": "shout" }))["deleted"],
        json!(false)
    );
    assert_eq!(run("patterns", json!({ "search": "shout" })), json!([]));
}

#[test]
fn a_pattern_that_misses_its_sample_is_not_saved() {
    let result = run(
        "save-pattern",
        json!({ "name": "digits-only", "pattern": r"^\d+$", "sample": "12a" }),
    );
    assert_eq!(result["code"], json!("sample-does-not-match"), "{result}");
    let bad_name = run(
        "save-pattern",
        json!({ "name": "Digits", "pattern": r"\d" }),
    );
    assert_eq!(bad_name["code"], json!("invalid-argument"), "{bad_name}");
    assert_eq!(
        run("patterns", json!({ "search": "digits-only" })),
        json!([])
    );
}

#[test]
fn matches_carry_lines_and_columns_in_characters() {
    let result = run(
        "find-all",
        json!({ "pattern": r"\d+", "haystack": "é1\nab22\n333" }),
    );
    let rows: Vec<(u64, u64, String)> = result["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["line"].as_u64().unwrap(),
                m["column"].as_u64().unwrap(),
                m["text"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, 2, "1".into()),
            (2, 3, "22".into()),
            (3, 1, "333".into())
        ]
    );
    // Byte offsets: `é` is two bytes.
    assert_eq!(result["matches"][0]["start"], json!(2));
}

#[test]
fn a_limit_truncates_and_says_so() {
    let result = run(
        "find-all",
        json!({ "pattern": "a", "haystack": "aaaa", "limit": 2 }),
    );
    assert_eq!(result["count"], json!(2));
    assert_eq!(result["truncated"], json!(true));
}

#[test]
fn captures_all_has_a_record_per_match_and_a_row_per_group() {
    let result = run(
        "captures-all",
        json!({ "pattern": r"(?P<word>[a-z]+)(\d)?", "haystack": "ab1 cd" }),
    );
    assert_eq!(result["columns"], json!(["match", "text", "word", "2"]));
    assert_eq!(
        result["records"][1],
        json!({ "match": "1", "text": "cd", "word": "cd", "2": "" })
    );
    // Two matches of three groups.
    assert_eq!(result["groups"].as_array().unwrap().len(), 6);
    assert_eq!(result["groups"][5]["matched"], json!(false));
}

#[test]
fn replace_expands_groups_unless_literal_and_counts_up_to_its_limit() {
    let expanded = run(
        "replace",
        json!({ "pattern": r"(\w+)@(\w+)", "haystack": "a@b c@d", "replacement": "$2 at $1" }),
    );
    assert_eq!(expanded["text"], json!("b at a d at c"));
    assert_eq!(expanded["replacements"], json!(2));
    let literal = run(
        "replace",
        json!({ "pattern": "x", "haystack": "xx", "replacement": "$1", "literal": true, "limit": 1 }),
    );
    assert_eq!(literal["text"], json!("$1x"));
    assert_eq!(literal["replacements"], json!(1));
}

#[test]
fn split_keeps_empty_pieces_and_their_offsets() {
    let result = run(
        "split",
        json!({ "pattern": ",", "haystack": "a,,b", "limit": 2 }),
    );
    let texts: Vec<&str> = result["pieces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["a", ",b"]);
    assert_eq!(result["pieces"][1]["start"], json!(2));
}

#[test]
fn a_set_refuses_the_pattern_that_does_not_compile_by_position() {
    let result = run(
        "set-matches",
        json!({ "patterns": ["ok", "(bad"], "haystack": "ok" }),
    );
    assert_eq!(result["code"], json!("invalid-pattern"), "{result}");
    assert!(result["message"].as_str().unwrap().starts_with("pattern 1"));
    let empty = run("set-matches", json!({ "patterns": [], "haystack": "x" }));
    assert_eq!(empty["code"], json!("invalid-argument"));
}

#[test]
fn check_reads_the_patterns_properties() {
    let literal = run("check", json!({ "pattern": "^abc$" }));
    assert_eq!(literal["anchored_start"], json!(true), "{literal}");
    assert_eq!(literal["anchored_end"], json!(true));
    assert_eq!(
        (literal["min_len"].clone(), literal["max_len"].clone()),
        (json!(3), json!(3))
    );
    let optional = run("check", json!({ "pattern": "a(b)?" }));
    assert_eq!(optional["static_group_count"], json!(null), "{optional}");
    assert_eq!(optional["groups"][1], json!({ "index": 1, "name": null }));
}

#[test]
fn a_start_inside_a_character_is_refused() {
    let result = run(
        "find",
        json!({ "pattern": ".", "haystack": "é", "start": 1 }),
    );
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
}

/// Verbs are strict: an argument the schema does not name is refused, not
/// ignored (a typo must not read as a default).
#[test]
fn a_misspelt_argument_is_refused() {
    let result = run("find", json!({ "pattern": "a", "hastack": "a" }));
    assert_eq!(result["ok"], json!(false), "{result}");
    assert_eq!(result["code"], json!("invalid-argument"), "{result}");
}

#[test]
fn a_set_searches_from_start_and_says_whether_all_matched() {
    let all = run(
        "set-matches",
        json!({ "patterns": ["a", "b"], "haystack": "ab" }),
    );
    assert_eq!(all["matched_all"], json!(true), "{all}");
    let later = run(
        "set-matches",
        json!({ "patterns": ["a", "b"], "haystack": "ab", "start": 1 }),
    );
    assert_eq!(later["matched_all"], json!(false), "{later}");
    assert_eq!(later["patterns"][1]["matched"], json!(true));
}

/// A refusal says it refused and why, and nothing that reads as an answer:
/// a refused `check` has no `literal: false`, a refused `find-all` no
/// `count: 0`.
#[test]
fn a_refusal_carries_no_answer_fields() {
    let refusal_keys = ["ok", "code", "message", "at", "diagram"];
    for (verb, args) in [
        ("check", json!({ "pattern": "(" })),
        ("find-all", json!({ "pattern": "(", "haystack": "x" })),
        (
            "replace",
            json!({ "pattern": "(", "haystack": "x", "replacement": "y" }),
        ),
        ("delete-pattern", json!({ "name": "Not A Name" })),
    ] {
        let result = run(verb, args);
        assert_eq!(result["ok"], json!(false), "{verb}: {result}");
        let extra: Vec<&String> = result
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| !refusal_keys.contains(&k.as_str()))
            .collect();
        assert!(
            extra.is_empty(),
            "{verb} refused with answer fields {extra:?}: {result}"
        );
    }
    // An answer still carries its fields, a "nothing found" one included.
    let none = run("find", json!({ "pattern": "z", "haystack": "x" }));
    assert_eq!(
        (none["ok"].clone(), none["found"].clone()),
        (json!(true), json!(false))
    );
}
