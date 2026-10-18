//! Kinds that evolve, and what a build does not understand (docs/plan-koine-
//! trust.md TR-D12, TR-D13): writes are stamped with their kind's schema
//! version, and fields and kinds a build does not know come back untouched.

use std::collections::BTreeMap;

use impress_item::kind::{self, FieldDef, FieldType, KindSchema};
use impress_item::{AttributedStore, OperationType, Value};

use crate::fixture::*;

/// The kind these cases register; its schema is the same in every backend.
const STAMPED: &str = "conformance/stamped@1.0.0";

fn register_stamped() {
    let _ = kind::register(KindSchema {
        name: STAMPED.into(),
        version: "1.2.0".into(),
        fields: vec![
            FieldDef::required("title", FieldType::String),
            FieldDef::optional("author_text", FieldType::String),
        ],
        enforce: false,
    });
}

/// An item of a registered kind is stamped with its schema's version; one
/// the writer stamped keeps the writer's.
pub fn version_is_stamped_on_write(store: &dyn AttributedStore) {
    register_stamped();
    let id = insert(store, STAMPED, "Stamped");
    let got = store.get(id).unwrap().expect("the item");
    assert_eq!(got.version.as_deref(), Some("1.2.0"));
    let mut own = item(STAMPED, "Own");
    own.version = Some("1.0.0".into());
    let own_id = store.insert(own).unwrap();
    assert_eq!(
        store.get(own_id).unwrap().unwrap().version.as_deref(),
        Some("1.0.0"),
        "a writer's own stamp is kept"
    );
}

/// An operation on an item of a registered kind records the schema version
/// it was written under.
pub fn op_schema_version_is_stamped(store: &dyn AttributedStore) {
    register_stamped();
    let id = insert(store, STAMPED, "Edited");
    store
        .apply_operation(spec(
            id,
            OperationType::SetPayload("title".into(), text("Edited again")),
        ))
        .unwrap();
    let ops = operations_of(store, id);
    let op = ops.last().expect("one operation");
    assert_eq!(
        op.payload.get("op_schema_version"),
        Some(&text("1.2.0")),
        "{:?}",
        op.payload
    );
}

/// Payload fields no schema names — a newer writer's — come back exactly,
/// nested ones included, and survive an edit of another field.
pub fn unknown_payload_fields_survive(store: &dyn AttributedStore) {
    register_stamped();
    let mut it = item(STAMPED, "Future");
    it.payload.insert(
        "from_a_newer_build".into(),
        Value::Object(BTreeMap::from([(
            "nested".to_owned(),
            Value::Array(vec![text("kept"), Value::Int(7)]),
        )])),
    );
    let id = store.insert(it.clone()).unwrap();
    store
        .apply_operation(spec(
            id,
            OperationType::SetPayload("title".into(), text("Renamed")),
        ))
        .unwrap();
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(
        got.payload.get("from_a_newer_build"),
        it.payload.get("from_a_newer_build")
    );
}

/// An item of a kind no schema and no manifest names is read, listed and
/// edited like any other.
pub fn unknown_kind_round_trips(store: &dyn AttributedStore) {
    let kind_name = "a-newer-app/unheard-of@3.1.0";
    let id = insert(store, kind_name, "Unheard of");
    let got = store.get(id).unwrap().expect("read back");
    assert_eq!(got.schema.as_str(), kind_name);
    assert_eq!(got.version, None, "no schema, no stamp");
    let listed = store.query(&all_of(kind_name)).unwrap();
    assert_eq!(titles(&listed), ["Unheard of"]);
    store
        .apply_operation(spec(id, OperationType::AddTag("seen".into())))
        .unwrap();
    assert_eq!(store.get(id).unwrap().unwrap().tags, ["seen"]);
}

/// An item that arrives before its kind's schema is kept as it came; the
/// schema arriving later changes nothing already stored.
pub fn item_before_its_kind_schema(store: &dyn AttributedStore) {
    let kind_name = format!("conformance/late-{}@1.0.0", uuid::Uuid::new_v4().simple());
    let id = insert(store, &kind_name, "Early");
    kind::register(KindSchema {
        name: kind_name.clone(),
        version: "1.0.0".into(),
        fields: vec![FieldDef::required("title", FieldType::String)],
        enforce: false,
    })
    .unwrap();
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.version, None, "stored before the schema, so unstamped");
    assert_eq!(got.payload.get("title"), Some(&text("Early")));
    let later = insert(store, &kind_name, "Later");
    assert_eq!(
        store.get(later).unwrap().unwrap().version.as_deref(),
        Some("1.0.0")
    );
}

/// Envelope fields this build does not know — a newer build's — come back
/// exactly, and survive an edit.
pub fn unknown_envelope_fields_survive(store: &dyn AttributedStore) {
    let mut it = item("test", "Envelope");
    it.extra.insert(
        "a_newer_envelope_field".into(),
        serde_json::json!({"kept": [1, "two"]}),
    );
    let id = store.insert(it.clone()).unwrap();
    store
        .apply_operation(spec(id, OperationType::AddTag("edited".into())))
        .unwrap();
    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.extra, it.extra, "unknown envelope fields are kept");
    for op in operations_of(store, id) {
        assert!(
            op.extra.is_empty(),
            "an operation row grows no envelope fields of its own: {:?}",
            op.extra
        );
    }
}

/// An enforced kind refuses an insert whose payload does not match its
/// schema, and writes nothing; a matching one goes in.
pub fn an_enforced_kind_refuses_an_invalid_insert(store: &dyn AttributedStore) {
    let kind_name = "conformance/enforced@1.0.0";
    let _ = kind::register(KindSchema {
        name: kind_name.into(),
        version: "1.0.0".into(),
        fields: vec![
            FieldDef::required("title", FieldType::String),
            FieldDef::optional("year", FieldType::Int),
        ],
        enforce: true,
    });
    let mut bad = item(kind_name, "Bad");
    bad.payload.insert("year".into(), text("not a year"));
    let refused = store.insert(bad.clone());
    assert!(
        matches!(refused, Err(impress_item::StoreError::Validation(_))),
        "{refused:?}"
    );
    assert!(store.get(bad.id).unwrap().is_none(), "nothing was written");
    let good = insert(store, kind_name, "Good");
    assert!(store.get(good).unwrap().is_some());
}
