//! The surface HTTP route table (docs/plan-koine-host.md H-D3): the pure
//! mapping from `(method, path, body)` of one `/api/surface/…` request to the
//! verb it runs, so the Swift FFI (`impress-store-ffi`'s `SharedSurface`) and a
//! headless host call one table and cannot disagree about what a path means.
//!
//! Pure: no store, no runtime, no I/O. Running the verb, the query-string
//! arguments and the reply status are the host's.

use serde_json::{json, Value};

/// The route table: `(verb, id from the path, arguments from the body)` for
/// one `/api/surface/…` request, given the path segments after `/api/surface`
/// (`rest`). `None` is no such route.
///
/// Two bodies have a shorthand the verb's own shape does not: a spec on its
/// own (it has a `surface` key; the arguments never do) is `{"spec": …}`, and
/// an event on its own (a `widget` key) is `{"event": …}`.
pub fn route(
    method: &str,
    rest: &[&str],
    body: Option<Value>,
) -> Option<(&'static str, Option<String>, Option<Value>)> {
    let id = |s: &&str| Some(s.to_string());
    Some(match (method, rest) {
        ("GET", []) => ("surface_list", None, body),
        ("POST", []) => ("surface_create", None, spec_body(body)),
        ("GET", ["schema"]) => ("surface_schema", None, body),
        ("GET", ["examples"]) => ("surface_examples", None, body),
        ("POST", ["validate"]) => ("surface_validate", None, spec_body(body)),
        ("GET", [s]) => ("surface_get", id(s), body),
        ("PUT", [s]) => ("surface_update", id(s), spec_body(body)),
        ("DELETE", [s]) => ("surface_delete", id(s), body),
        ("POST", [s, "show"]) => ("surface_show", id(s), body),
        ("GET", [s, "render"]) => ("surface_render", id(s), body),
        ("POST", [s, "dispatch"]) => ("surface_dispatch", id(s), event_body(body)),
        ("GET", [s, "state"]) => ("surface_state_get", id(s), body),
        ("PUT", [s, "state"]) => ("surface_state_set", id(s), body),
        ("GET", [s, "events"]) => ("surface_events", id(s), body),
        ("GET", [s, "wait"]) => ("surface_wait", id(s), body),
        _ => return None,
    })
}

/// A spec on its own is `{"spec": …}`.
fn spec_body(body: Option<Value>) -> Option<Value> {
    match body {
        Some(v) if v.get("surface").is_some() => Some(json!({ "spec": v })),
        other => other,
    }
}

/// An event on its own is `{"event": …}`.
fn event_body(body: Option<Value>) -> Option<Value> {
    match body {
        Some(v) if v.get("widget").is_some() => Some(json!({ "event": v })),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0f0f0f0f-0000-4000-8000-000000000001";

    fn verb(method: &str, rest: &[&str]) -> Option<(&'static str, Option<String>)> {
        route(method, rest, None).map(|(verb, id, _)| (verb, id))
    }

    /// Every route the docs list, and the id each takes from the path.
    #[test]
    fn every_documented_route_names_its_verb_and_id() {
        let table: &[(&str, &[&str], &str, bool)] = &[
            ("GET", &[], "surface_list", false),
            ("POST", &[], "surface_create", false),
            ("GET", &["schema"], "surface_schema", false),
            ("GET", &["examples"], "surface_examples", false),
            ("POST", &["validate"], "surface_validate", false),
            ("GET", &[ID], "surface_get", true),
            ("PUT", &[ID], "surface_update", true),
            ("DELETE", &[ID], "surface_delete", true),
            ("POST", &[ID, "show"], "surface_show", true),
            ("GET", &[ID, "render"], "surface_render", true),
            ("POST", &[ID, "dispatch"], "surface_dispatch", true),
            ("GET", &[ID, "state"], "surface_state_get", true),
            ("PUT", &[ID, "state"], "surface_state_set", true),
            ("GET", &[ID, "events"], "surface_events", true),
            ("GET", &[ID, "wait"], "surface_wait", true),
        ];
        for (method, rest, expected, has_id) in table {
            let (name, id) = verb(method, rest).unwrap_or_else(|| {
                panic!("{method} /api/surface/{} is not a route", rest.join("/"))
            });
            assert_eq!(name, *expected, "{method} {rest:?}");
            assert_eq!(id.is_some(), *has_id, "{method} {rest:?}");
            if *has_id {
                assert_eq!(id.as_deref(), Some(ID));
            }
        }
    }

    /// A method the path does not take, and a path nothing serves, are no
    /// route at all (the host answers `not-found`), never a guess.
    #[test]
    fn an_unknown_method_or_path_is_no_route() {
        for (method, rest) in [
            ("DELETE", vec![]),
            ("PATCH", vec![ID]),
            ("GET", vec![ID, "show"]),
            ("POST", vec![ID, "render"]),
            ("GET", vec![ID, "state", "extra"]),
            ("POST", vec!["schema"]),
            ("GET", vec![ID, "nonsense"]),
        ] {
            assert!(
                verb(method, &rest).is_none(),
                "{method} {rest:?} must not be a route"
            );
        }
    }

    /// A spec on its own (it has a `surface` key) is wrapped as `{"spec": …}`
    /// on every route that takes one; an arguments object passes through.
    #[test]
    fn a_bare_spec_body_becomes_the_spec_argument() {
        let spec = json!({"surface": 1, "name": "demo"});
        for (method, rest) in [
            ("POST", vec![]),
            ("POST", vec!["validate"]),
            ("PUT", vec![ID]),
        ] {
            let (_, _, body) = route(method, &rest, Some(spec.clone())).unwrap();
            assert_eq!(body, Some(json!({ "spec": spec })), "{method} {rest:?}");
        }
        let args = json!({"spec": spec, "name": "n"});
        let (_, _, body) = route("POST", &[], Some(args.clone())).unwrap();
        assert_eq!(body, Some(args), "arguments are not re-wrapped");
        let (_, _, body) = route("POST", &[], None).unwrap();
        assert_eq!(body, None);
    }

    /// An event on its own (it has a `widget` key) is wrapped as
    /// `{"event": …}` on dispatch, and only there.
    #[test]
    fn a_bare_event_body_becomes_the_event_argument() {
        let event = json!({"widget": "go", "kind": "click"});
        let (_, _, body) = route("POST", &[ID, "dispatch"], Some(event.clone())).unwrap();
        assert_eq!(body, Some(json!({ "event": event })));
        let args = json!({"event": event});
        let (_, _, body) = route("POST", &[ID, "dispatch"], Some(args.clone())).unwrap();
        assert_eq!(body, Some(args));
        // Other routes leave a `widget`-bearing body alone.
        let (_, _, body) = route("PUT", &[ID, "state"], Some(event.clone())).unwrap();
        assert_eq!(body, Some(event));
    }
}
