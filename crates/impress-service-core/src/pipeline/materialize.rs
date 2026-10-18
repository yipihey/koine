//! Materialised rows: a verb's rows become records (plan-koine-live L-D4).
//!
//! A linked verb that declares `returns_kind` and `rows(…)` answers rows. A
//! host that wants them kept installs a [`Materializer`] ([`install`]); after
//! each such verb answers `ok`, [`after_verb`] hands it the rows and the host
//! upserts them as records, so a pane can query, tag, star and select them
//! and they are there offline.
//!
//! The seam is the shape of the runner's and the blob reader's: a process
//! slot the host fills and nothing else does. A bare CLI or MCP process
//! installs no materialiser and [`after_verb`] does nothing for it.
//!
//! [`after_verb`] runs after the handler, outside the effects check (the
//! verb declared no write; the host's store is the host's to keep) and in the
//! host only. It never fails the verb: a materialiser that errs is a
//! `tracing::warn`, and the caller still has the rows it asked for.

use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::descriptor::{row_layout, RowSchema, VerbDescriptor};

/// What the host does with a verb's rows.
pub trait Materializer: Send + Sync {
    /// Upsert `rows_value` (each an object) as records of `kind`, `rows`
    /// naming which field is the row's id; `verb` is the qualified name of
    /// the verb that produced them. Answers how many rows were upserted.
    fn materialize(
        &self,
        kind: &str,
        rows: &RowSchema,
        verb: &str,
        rows_value: &[Value],
    ) -> Result<usize, String>;
}

static SLOT: RwLock<Option<Arc<dyn Materializer>>> = RwLock::new(None);

/// Install the process's materialiser. A later install replaces the earlier.
pub fn install(m: Arc<dyn Materializer>) {
    *SLOT.write().unwrap_or_else(|e| e.into_inner()) = Some(m);
}

/// The installed materialiser, if any.
pub fn installed() -> Option<Arc<dyn Materializer>> {
    SLOT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Remove the installed materialiser (tests).
pub fn clear() {
    *SLOT.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The rows `result` carries, when `verb` declares `returns_kind` and
/// `rows(…)` and the result is `ok`: the result itself when it is an array,
/// else the `rows` array of the `{rows, next}` envelope (L-D5), else the one
/// array property the verb's output schema names as its rows. Only objects
/// are rows. `None` when the verb declares no rows, the result refused
/// (`"ok": false`), or no rows are in it.
pub fn rows_of(verb: &VerbDescriptor, result: &Value) -> Option<Vec<Value>> {
    verb.returns_kind?;
    verb.rows?;
    if result.get("ok").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let array = match result {
        Value::Array(items) => items,
        Value::Object(fields) => match fields.get("rows") {
            Some(Value::Array(items)) => items,
            _ => {
                let container = row_layout(&(verb.output_schema)())?.container?;
                match fields.get(&container)? {
                    Value::Array(items) => items,
                    _ => return None,
                }
            }
        },
        _ => return None,
    };
    Some(array.iter().filter(|r| r.is_object()).cloned().collect())
}

/// The pipeline's one call site, after a linked verb's handler ran: hand the
/// verb's rows to the installed materialiser, if there is one and the verb
/// has rows. Never fails the verb; an error is logged.
pub fn after_verb(verb: &VerbDescriptor, result: &Value) {
    let (Some(kind), Some(schema)) = (verb.returns_kind, verb.rows) else {
        return;
    };
    let Some(materializer) = installed() else {
        return;
    };
    let Some(rows) = rows_of(verb, result) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    match materializer.materialize(kind, &schema, verb.name, &rows) {
        Ok(count) => tracing::debug!(verb = verb.name, kind, count, "materialised rows"),
        Err(message) => {
            tracing::warn!(verb = verb.name, kind, %message, "materialising rows failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effects, Safety, SafetyClass, ServiceFuture, Source};
    use serde_json::json;
    use std::sync::Mutex;

    // The slot is process-wide; these cases take turns.
    static TURN: Mutex<()> = Mutex::new(());

    type Call = (String, String, String, Vec<Value>);

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<Call>>,
        fail: bool,
    }

    impl Materializer for Recorder {
        fn materialize(
            &self,
            kind: &str,
            rows: &RowSchema,
            verb: &str,
            rows_value: &[Value],
        ) -> Result<usize, String> {
            self.calls.lock().unwrap().push((
                kind.into(),
                rows.id.into(),
                verb.into(),
                rows_value.to_vec(),
            ));
            if self.fail {
                Err("store is sad".into())
            } else {
                Ok(rows_value.len())
            }
        }
    }

    fn array_schema() -> Value {
        json!({ "type": "array", "items": { "type": "object", "properties": { "id": {}, "t": {} } } })
    }

    fn wrapped_schema() -> Value {
        json!({ "type": "object", "properties": { "issues": array_schema() } })
    }

    fn handler(_: Value) -> ServiceFuture {
        Box::pin(async { Ok(json!(null)) })
    }

    static ROWS: VerbDescriptor = VerbDescriptor {
        name: "mat-fixture_list",
        service: "mat-fixture",
        method: "list",
        description: "fixture",
        input_schema: array_schema,
        output_schema: array_schema,
        safety: Safety {
            class: SafetyClass::ReadOnly,
            idempotent: true,
        },
        since: "0.1.0",
        effects: Effects::NONE,
        deprecated: None,
        aliases: &[],
        supports: &[],
        adjective: None,
        examples: &[],
        strict: false,
        budget_ms: None,
        replay_full: false,
        returns_kind: Some("paper"),
        rows: Some(RowSchema {
            id: "id",
            title: Some("t"),
            columns: &[],
        }),
        secrets: &[],
        source: Source::Linked,
        handler,
        consent: crate::Consent::DEFAULT,
    };

    static WRAPPED: VerbDescriptor = VerbDescriptor {
        name: "mat-fixture_wrapped",
        output_schema: wrapped_schema,
        ..ROWS
    };

    static NO_ROWS: VerbDescriptor = VerbDescriptor {
        name: "mat-fixture_plain",
        returns_kind: None,
        rows: None,
        ..ROWS
    };

    static KIND_ONLY: VerbDescriptor = VerbDescriptor {
        name: "mat-fixture_kind-only",
        rows: None,
        ..ROWS
    };

    #[test]
    fn rows_of_reads_an_array_an_envelope_and_the_one_array_property() {
        let rows = json!([{ "id": "a" }, { "id": "b" }]);
        assert_eq!(rows_of(&ROWS, &rows).unwrap().len(), 2);
        // The `{rows, next}` envelope of L-D5.
        let envelope = json!({ "rows": [{ "id": "a" }], "next": "c2" });
        assert_eq!(
            rows_of(&ROWS, &envelope).unwrap(),
            vec![json!({ "id": "a" })]
        );
        // The property the output schema names.
        let wrapped = json!({ "ok": true, "issues": [{ "id": "a" }, { "id": "b" }] });
        assert_eq!(rows_of(&WRAPPED, &wrapped).unwrap().len(), 2);
        // Only objects are rows.
        assert_eq!(
            rows_of(&ROWS, &json!([{ "id": "a" }, 7, "x"]))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rows_of_answers_none_without_rows_or_after_a_refusal() {
        let rows = json!([{ "id": "a" }]);
        assert!(rows_of(&NO_ROWS, &rows).is_none());
        assert!(rows_of(&KIND_ONLY, &rows).is_none());
        let refused = json!({ "ok": false, "code": "nope", "rows": [{ "id": "a" }] });
        assert!(rows_of(&ROWS, &refused).is_none());
        assert!(rows_of(&ROWS, &json!({ "ok": true })).is_none());
        assert!(rows_of(&ROWS, &json!("text")).is_none());
        // An object that is not an envelope, from a verb whose schema is an array.
        assert!(rows_of(&ROWS, &json!({ "other": [{ "id": "a" }] })).is_none());
    }

    #[test]
    fn after_verb_hands_the_rows_to_the_installed_materialiser() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let recorder = Arc::new(Recorder::default());
        install(recorder.clone());
        after_verb(&ROWS, &json!([{ "id": "a" }, { "id": "b" }]));
        after_verb(&NO_ROWS, &json!([{ "id": "a" }]));
        after_verb(&ROWS, &json!({ "ok": false }));
        after_verb(&ROWS, &json!([]));
        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "only the one verb with rows and some rows");
        let (kind, id_field, verb, rows) = &calls[0];
        assert_eq!(
            (kind.as_str(), id_field.as_str(), verb.as_str()),
            ("paper", "id", "mat-fixture_list")
        );
        assert_eq!(rows.len(), 2);
        drop(calls);
        clear();
        assert!(installed().is_none());
    }

    #[test]
    fn a_failing_materialiser_and_an_absent_one_never_fail_the_verb() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        after_verb(&ROWS, &json!([{ "id": "a" }])); // nothing installed: nothing happens
        let recorder = Arc::new(Recorder {
            fail: true,
            ..Recorder::default()
        });
        install(recorder.clone());
        after_verb(&ROWS, &json!([{ "id": "a" }])); // returns (), no panic
        assert_eq!(recorder.calls.lock().unwrap().len(), 1);
        clear();
    }
}
