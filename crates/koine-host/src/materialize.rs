//! The host's materialiser: a verb's rows become records (plan-koine-live
//! L-D4).
//!
//! `impress_service_core::pipeline::materialize` is the seam; this is what a
//! host plugs into it. [`StoreMaterializer`] upserts each row a
//! `returns_kind` + `rows(…)` verb answers as an item of the kind's schema:
//!
//! * **id**: `uuid5(koine-rows, "<kind>:<row id>")`, where the row id is the
//!   text of the row's `rows.id` field. The same row read twice is the same
//!   record, on every host, so tags, flags and references put on it survive
//!   the next refresh. A row without an id is skipped and counted in a
//!   warning.
//! * **schema ref**: the kind manifest's first ref for the kind (the
//!   [`crate::manifest::from_inventory`] manifest maps each such kind to
//!   itself), else the kind.
//! * **payload**: the row's fields as they came, plus `_verb`, the qualified
//!   name of the verb that last answered it.
//! * **update**: an existing record gets a `SetPayload` for each field that
//!   changed or is new, and nothing else: its tags, stars, flags and
//!   references are untouched, an unchanged row is no write at all, and a
//!   field the verb stopped sending stays. A record whose row is gone is
//!   never deleted: a verb's page is not the whole set.
//!
//! Install it with [`install`] once the store and manifest are known; the
//! orchestration (`serve`) does that. A bare CLI or MCP process installs
//! none and the pipeline does nothing for it.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use impress_item::{
    ActorKind, AttributedStore, FieldMutation, Item, ItemId, Priority, SchemaRef, Value, Visibility,
};
use impress_pane_query::KindManifest;
use impress_service_core::pipeline::materialize::{self, Materializer};
use impress_service_core::RowSchema;
use serde_json::Value as Json;
use uuid::Uuid;

/// The author of a materialised row.
pub const AUTHOR: &str = "system:host";

/// Install a [`StoreMaterializer`] over `store` as the process's
/// materialiser.
pub fn install(store: Arc<dyn AttributedStore>, manifest: Arc<KindManifest>) {
    materialize::install(Arc::new(StoreMaterializer::new(store, manifest)));
}

/// Upserts a verb's rows into the store; see the module documentation.
pub struct StoreMaterializer {
    store: Arc<dyn AttributedStore>,
    manifest: Arc<KindManifest>,
}

impl StoreMaterializer {
    /// A materialiser over `store`, naming schema refs by `manifest`.
    pub fn new(store: Arc<dyn AttributedStore>, manifest: Arc<KindManifest>) -> Self {
        Self { store, manifest }
    }

    /// The record id of row `row_id` of `kind`.
    pub fn record_id(kind: &str, row_id: &str) -> ItemId {
        uuid5(namespace(), format!("{kind}:{row_id}").as_bytes())
    }

    fn schema_of(&self, kind: &str) -> SchemaRef {
        let name = self
            .manifest
            .schema_refs(kind)
            .and_then(|refs| refs.first())
            .map_or(kind, String::as_str);
        SchemaRef::from_stored(name.to_owned())
    }

    fn upsert(
        &self,
        schema: &SchemaRef,
        id: ItemId,
        payload: BTreeMap<String, Value>,
    ) -> Result<(), String> {
        let existing = self.store.get(id).map_err(|e| e.to_string())?;
        let Some(existing) = existing else {
            let now = chrono::Utc::now();
            let item = Item {
                id,
                schema: schema.clone(),
                payload,
                created: now,
                modified: now,
                author: AUTHOR.into(),
                author_kind: ActorKind::System,
                logical_clock: 0,
                origin: None,
                canonical_id: None,
                tags: vec![],
                flag: None,
                is_read: false,
                is_starred: false,
                priority: Priority::default(),
                visibility: Visibility::Private,
                message_type: None,
                produced_by: None,
                version: None,
                batch_id: None,
                references: vec![],
                parent: None,
            };
            return self
                .store
                .insert(item)
                .map(|_| ())
                .map_err(|e| e.to_string());
        };
        let changes: Vec<FieldMutation> = payload
            .into_iter()
            .filter(|(field, value)| existing.payload.get(field) != Some(value))
            .map(|(field, value)| FieldMutation::SetPayload(field, value))
            .collect();
        if changes.is_empty() {
            return Ok(());
        }
        self.store.update(id, changes).map_err(|e| e.to_string())
    }
}

impl Materializer for StoreMaterializer {
    fn materialize(
        &self,
        kind: &str,
        rows: &RowSchema,
        verb: &str,
        rows_value: &[Json],
    ) -> Result<usize, String> {
        let schema = self.schema_of(kind);
        let (mut upserted, mut skipped, mut failed) = (0usize, 0usize, 0usize);
        let mut first_error = None;
        for row in rows_value {
            let Some(fields) = row.as_object() else {
                skipped += 1;
                continue;
            };
            let Some(row_id) = fields.get(rows.id).and_then(id_text) else {
                skipped += 1;
                continue;
            };
            let mut payload: BTreeMap<String, Value> = BTreeMap::new();
            for (field, value) in fields {
                match serde_json::from_value::<Value>(value.clone()) {
                    Ok(value) => {
                        payload.insert(field.clone(), value);
                    }
                    Err(e) => tracing::warn!(verb, field, error = %e, "row field not kept"),
                }
            }
            payload.insert("_verb".into(), Value::String(verb.to_owned()));
            match self.upsert(&schema, Self::record_id(kind, &row_id), payload) {
                Ok(()) => upserted += 1,
                Err(e) => {
                    failed += 1;
                    first_error.get_or_insert(e);
                }
            }
        }
        if skipped > 0 {
            tracing::warn!(
                verb,
                kind,
                skipped,
                id_field = rows.id,
                "rows without an id (`rows.id`) were not materialised"
            );
        }
        match first_error {
            Some(e) => Err(format!(
                "{failed} of {} rows of {kind} not kept ({upserted} were): {e}",
                rows_value.len()
            )),
            None => Ok(upserted),
        }
    }
}

/// A row id as text: a string, a number or a boolean; anything else is no id.
fn id_text(value: &Json) -> Option<String> {
    match value {
        Json::String(s) if !s.is_empty() => Some(s.clone()),
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// The namespace of materialised rows: `uuid5(OID namespace, "koine-rows")`.
fn namespace() -> Uuid {
    static NAMESPACE: OnceLock<Uuid> = OnceLock::new();
    *NAMESPACE.get_or_init(|| uuid5(Uuid::NAMESPACE_OID, b"koine-rows"))
}

/// A version-5 (SHA-1, name-based) uuid, RFC 4122 § 4.3. Written out here
/// because the workspace builds `uuid` without its `v5` feature, and turning
/// it on would change the feature set `koine-workspace-hack` pins.
fn uuid5(namespace: Uuid, name: &[u8]) -> Uuid {
    let mut message = namespace.as_bytes().to_vec();
    message.extend_from_slice(name);
    let digest = sha1(&message);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut data = message.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());
    let (blocks, _) = data.as_chunks::<64>();
    for block in blocks {
        let mut w = [0u32; 80];
        for (slot, word) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (slot, add) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(add);
        }
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid5_matches_the_rfc_example() {
        // Python's uuid.uuid5(uuid.NAMESPACE_DNS, "www.example.com").
        assert_eq!(
            uuid5(Uuid::NAMESPACE_DNS, b"www.example.com").to_string(),
            "2ed6657d-e927-568b-95e1-2665a8aea6a2"
        );
        // A message longer than one block (the padding crosses a boundary).
        let long = vec![b'a'; 200];
        assert_eq!(uuid5(Uuid::NAMESPACE_DNS, &long).get_version_num(), 5);
    }

    #[test]
    fn a_record_id_is_stable_and_depends_on_kind_and_row() {
        let a = StoreMaterializer::record_id("paper", "2024A");
        assert_eq!(a, StoreMaterializer::record_id("paper", "2024A"));
        assert_ne!(a, StoreMaterializer::record_id("paper", "2024B"));
        assert_ne!(a, StoreMaterializer::record_id("issue", "2024A"));
    }

    #[test]
    fn row_ids_are_text_numbers_or_booleans() {
        assert_eq!(id_text(&serde_json::json!("x")).as_deref(), Some("x"));
        assert_eq!(id_text(&serde_json::json!(42)).as_deref(), Some("42"));
        assert_eq!(id_text(&serde_json::json!("")), None);
        assert_eq!(id_text(&serde_json::json!(null)), None);
        assert_eq!(id_text(&serde_json::json!({})), None);
    }
}
