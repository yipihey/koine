//! The tour (docs/plan-koine-tour.md T-D1, T-D2): five surfaces seeded at
//! launch, idempotently, from spec files in this crate.
//!
//! Each spec is a JSON file under `surfaces/`, included at compile time
//! ([`specs`]), so the same document is what Tier A validates
//! (`tests/tour.rs`), what the simulator lane sees and what the phone draws.
//! [`seed`] goes through the in-process verb pipeline exactly as an agent
//! off the phone would — `impress-surface-service_surface-list`, then for
//! each spec not yet stored `…_surface-validate` and `…_surface-create`,
//! tagged [`TAG`] — and seeds the `note` records the Notes surface shows
//! when there are none. Nothing in `koine-host` knows the tour: the demo
//! binary and `koine-host --tour` call [`seed`] (or [`seed_blocking`]) once
//! the host is bound, i.e. once the process store
//! (`koine_store::process`) and the kind manifest are installed.
//!
//! | Surface | Shows | Over |
//! |---|---|---|
//! | Signal explorer | sliders, a verb-driven histogram plot | `series`, `histogram` |
//! | Live job | a background job's emitted lines, arriving | `tick` `in_background`, a `query` over `task`, `log` |
//! | Notes | collections of notes: rename, move, delete, add, write a body | `note` records; `note-get/add/rename/write/move/delete` |
//! | Map of this app | a graph of verbs, kinds and surfaces | `inventory` |
//! | Concierge | an agent in the loop (`crate::concierge`) | `surface_wait` → react → `surface_create` → `surface_show` |
//!
//! The Notes spec is the one spec seeding touches before storing: its
//! `state.open` (the tree's expanded ids) is set to the record ids of the
//! root notes, so the outline opens unfolded. The file keeps `[]`, because
//! the ids are only known once the notes exist.
//!
//! # A changed spec replaces the stored one (plan-koine-affordances A-D8)
//!
//! Each seeded surface carries, beside [`TAG`], a content tag
//! `tour-spec:<hex>` ([`spec_tag`]): the FNV-1a 64 hash of the spec file's
//! JSON in canonical form (keys sorted, no whitespace), taken before
//! [`prepared_spec`] fills Notes' `state.open`, so a store's own notes do not
//! make the hash differ on every launch. On [`seed`], a stored [`TAG`]
//! surface whose content tag is not the file's (or that has none: seeded by
//! 0.3.0 or earlier) has its spec replaced in place through
//! `…_surface-update` (same id, revision bumped) and its tag refreshed; the
//! report lists it under `updated`. A surface without [`TAG`] is never
//! touched, and the notes keep their records.
//!
//! # The layout (A-D4)
//!
//! [`seed_layout`] puts three surfaces side by side in the host's window,
//! once: when the live layout of `(app_id, device)` has no `surface` tile, it
//! shows Signal explorer, Notes and Live job in the panes whose roles are
//! `navigator`, `list` and `detail` (the default arrangement's three
//! columns) with `…_surface-show`, and gives the three equal shares with
//! `layout-service_resize`. A layout that already shows a surface is left as
//! the person arranged it. [`seed_with_layout`] is [`seed`] then this.

use impress_service_core::pipeline::{self, Call};
use impress_service_core::VerbDescriptor;
use serde::Serialize;
use serde_json::{json, Value};

use crate::notes;

/// The tag every tour surface is created with, and matched by on the next
/// launch.
pub const TAG: &str = "tour";

const SIGNAL_EXPLORER: &str = include_str!("../surfaces/signal-explorer.surface.json");
const LIVE_JOB: &str = include_str!("../surfaces/live-job.surface.json");
const NOTES: &str = include_str!("../surfaces/notes.surface.json");
const MAP: &str = include_str!("../surfaces/map.surface.json");

/// The surfaces' names, as the specs' `name` and as `surface_list` shows them.
pub const SIGNAL_EXPLORER_NAME: &str = "Signal explorer";
pub const LIVE_JOB_NAME: &str = "Live job";
pub const NOTES_NAME: &str = "Notes";
pub const MAP_NAME: &str = "Map of this app";

/// The prefix of the content tag (see the module docs, A-D8).
pub const SPEC_TAG_PREFIX: &str = "tour-spec:";

/// The verbs seeding calls, by their inventory names.
const LIST: &str = "impress-surface-service_surface-list";
const VALIDATE: &str = "impress-surface-service_surface-validate";
const CREATE: &str = "impress-surface-service_surface-create";
const UPDATE: &str = "impress-surface-service_surface-update";
const SHOW: &str = "impress-surface-service_surface-show";
const GET_LAYOUT: &str = "layout-service_get-layout";
const RESIZE: &str = "layout-service_resize";

/// The surfaces [`seed_layout`] shows, left to right, and the role of the
/// pane each replaces.
pub const LAYOUT: [(&str, &str); 3] = [
    (SIGNAL_EXPLORER_NAME, "navigator"),
    (NOTES_NAME, "list"),
    (LIVE_JOB_NAME, "detail"),
];

/// The four content surfaces of this module: `(name, spec JSON)`, in the
/// order the tour shows them. The concierge's spec is `crate::concierge`'s;
/// [`all_specs`] has all five.
pub fn specs() -> Vec<(&'static str, &'static str)> {
    vec![
        (SIGNAL_EXPLORER_NAME, SIGNAL_EXPLORER),
        (LIVE_JOB_NAME, LIVE_JOB),
        (NOTES_NAME, NOTES),
        (MAP_NAME, MAP),
    ]
}

/// Every surface [`seed`] creates: [`specs`] plus the concierge.
pub fn all_specs() -> Vec<(&'static str, &'static str)> {
    let mut all = specs();
    all.push((crate::concierge::NAME, crate::concierge::SPEC));
    all
}

/// The notes seeded when the store has none: `(key, parent key, title,
/// body)`. Two collections (the roots) with two notes each.
pub const SEED_NOTES: &[(&str, Option<&str>, &str, &str)] = &[
    (
        "research",
        None,
        "Research",
        "What this project is trying to find out, and the notes that get it \
         there. Drag a note here from another collection to file it.",
    ),
    (
        "ideas",
        Some("research"),
        "Ideas",
        "Raise the sine wave's frequency until the histogram stops looking \
         bimodal, and write down where that happens. Then try it with fewer \
         samples and see whether the answer moves.",
    ),
    (
        "experiments",
        Some("research"),
        "Experiments",
        "Run the live job with a longer interval and watch the log fill line \
         by line. Each line is a value the job emitted onto its row while this \
         page waited for it.",
    ),
    (
        "reading",
        None,
        "Reading",
        "Things to read, and what they were for. Select a note to edit its \
         body, and Save to write it back with note-write.",
    ),
    (
        "papers",
        Some("reading"),
        "Papers to read",
        "A short list of papers to come back to: the ones on local-first \
         software first, then the ones on structured editors.",
    ),
    (
        "koine",
        Some("reading"),
        "Notes on koine",
        "Every surface here is a JSON spec that only calls verbs. The tree \
         reports a rename, a move or a delete; a note verb does the work.",
    ),
];

/// What [`seed`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SeedReport {
    /// `(name, surface id)` of each surface created on this call.
    pub created: Vec<(String, String)>,
    /// `(name, surface id)` of each surface that was already there with
    /// this release's spec.
    pub existing: Vec<(String, String)>,
    /// `(name, surface id)` of each surface that was there with another
    /// spec, now replaced in place (A-D8).
    pub updated: Vec<(String, String)>,
    /// `(name, tile)` of each surface [`seed_layout`] put in the window on
    /// this call; empty when the layout already showed a surface.
    pub layout: Vec<(String, u64)>,
    /// How many note records were written (0 when notes already existed).
    pub notes_seeded: usize,
    /// Everything that went wrong, one line each; empty on a clean seed.
    pub errors: Vec<String>,
}

impl SeedReport {
    /// No errors.
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// The id of the surface named `name`, created, updated or existing.
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.created
            .iter()
            .chain(self.updated.iter())
            .chain(self.existing.iter())
            .find(|(n, _)| n == name)
            .map(|(_, id)| id.as_str())
    }
}

/// Seed the tour: the notes when there are none, then every surface of
/// [`all_specs`]: created when no surface stored under [`TAG`] has its name,
/// replaced in place when the stored one's content tag differs from the
/// file's (A-D8), else left alone. Idempotent: a second call creates and
/// updates nothing. Never panics; what failed is in the report's `errors`.
pub async fn seed() -> SeedReport {
    let mut report = SeedReport::default();
    seed_notes(&mut report);

    let existing = match list_tour_surfaces().await {
        Ok(existing) => existing,
        Err(e) => {
            report.errors.push(format!("{LIST}: {e}"));
            return report;
        }
    };

    for (name, json) in all_specs() {
        let tag = match spec_tag(json) {
            Ok(tag) => tag,
            Err(e) => {
                report.errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        let stored = existing.iter().find(|s| s.name == name);
        if let Some(stored) = stored.filter(|s| s.tags.contains(&tag)) {
            report.existing.push((name.to_string(), stored.id.clone()));
            continue;
        }
        let spec = match prepared_spec(name, json) {
            Ok(spec) => spec,
            Err(e) => {
                report.errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        match stored {
            Some(stored) => match update(name, stored, spec, &tag).await {
                Ok(()) => report.updated.push((name.to_string(), stored.id.clone())),
                Err(e) => report.errors.push(format!("{name}: {e}")),
            },
            None => match create(name, spec, &tag).await {
                Ok(id) => report.created.push((name.to_string(), id)),
                Err(e) => report.errors.push(format!("{name}: {e}")),
            },
        }
    }
    report
}

/// [`seed`], then [`seed_layout`] for `(app_id, device)` (`device` absent:
/// this device's, what a host's own routes use by default).
pub async fn seed_with_layout(app_id: &str, device: Option<&str>) -> SeedReport {
    let mut report = seed().await;
    if report.ok() {
        seed_layout(app_id, device, &mut report).await;
    }
    report
}

/// [`seed_with_layout`] from a thread that is not inside a Tokio runtime.
pub fn seed_with_layout_blocking(app_id: &str, device: Option<&str>) -> SeedReport {
    impress_service_core::runtime::block_on(seed_with_layout(app_id, device))
}

/// Show [`LAYOUT`]'s three surfaces side by side in `(app_id, device)`'s
/// window, once (see the module docs): nothing when the live layout already
/// has a `surface` tile. `report` names the surfaces (from a [`seed`]) and
/// receives the tiles shown, or what failed. Every write is a verb:
/// `layout-service_get-layout`, `…_surface-show`, `layout-service_resize`.
pub async fn seed_layout(app_id: &str, device: Option<&str>, report: &mut SeedReport) {
    let place = json!({ "app_id": app_id, "device": device });
    let tree = match invoke(GET_LAYOUT, place.clone()).await {
        Ok(tree) if tree["ok"] == json!(true) => tree,
        Ok(refused) => {
            report
                .errors
                .push(format!("{GET_LAYOUT}: {}", refused["message"]));
            return;
        }
        Err(e) => {
            report.errors.push(format!("{GET_LAYOUT}: {e}"));
            return;
        }
    };
    let tiles = tree["layout"]["tiles"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if tiles
        .values()
        .any(|tile| tile["pane"]["view_kind"] == json!("surface"))
    {
        return;
    }
    let mut shown = Vec::new();
    for (name, role) in LAYOUT {
        let Some(id) = report.id_of(name).map(str::to_string) else {
            report
                .errors
                .push(format!("layout: no {name} surface to show"));
            return;
        };
        let answer = invoke(
            SHOW,
            json!({ "id": id, "target": { "role": role }, "app_id": app_id, "device": device }),
        )
        .await;
        match answer {
            Ok(answer) if answer["ok"] == json!(true) => {
                if let Some(tile) = answer["tile"].as_u64() {
                    shown.push((name.to_string(), tile));
                }
            }
            Ok(refused) => {
                report.errors.push(format!(
                    "layout: {name} in the {role} pane: {} ({})",
                    refused["message"].as_str().unwrap_or("refused"),
                    refused["code"].as_str().unwrap_or("?")
                ));
                return;
            }
            Err(e) => {
                report.errors.push(format!("layout: {name}: {e}"));
                return;
            }
        }
    }
    // Equal columns: the container holding exactly these three tiles.
    let shown_tiles: Vec<u64> = shown.iter().map(|(_, tile)| *tile).collect();
    let container = tiles.iter().find_map(|(id, tile)| {
        let children: Vec<u64> = tile["container"]["linear"]["children"]
            .as_array()?
            .iter()
            .filter_map(Value::as_u64)
            .collect();
        let mut sorted = children.clone();
        sorted.sort_unstable();
        let mut wanted = shown_tiles.clone();
        wanted.sort_unstable();
        (sorted == wanted).then(|| id.parse::<u64>().ok()).flatten()
    });
    if let Some(container) = container {
        let resized = invoke(
            RESIZE,
            json!({ "app_id": app_id, "device": device, "container": container,
                    "shares": [1.0, 1.0, 1.0] }),
        )
        .await;
        match resized {
            Ok(answer) if answer["ok"] == json!(true) => {}
            Ok(refused) => report
                .errors
                .push(format!("{RESIZE}: {}", refused["message"])),
            Err(e) => report.errors.push(format!("{RESIZE}: {e}")),
        }
    }
    report.layout = shown;
}

/// The content tag of a spec file (A-D8): [`SPEC_TAG_PREFIX`] and the
/// FNV-1a 64 hash, in hex, of the file's JSON in canonical form.
pub fn spec_tag(json: &str) -> Result<String, String> {
    let spec: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let mut canonical = String::new();
    write_canonical(&spec, &mut canonical);
    Ok(format!(
        "{SPEC_TAG_PREFIX}{:016x}",
        fnv1a_64(canonical.as_bytes())
    ))
}

/// FNV-1a, 64 bits: stable across releases and platforms, which a
/// `std::hash::Hasher` is not promised to be.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

/// `value` as JSON with every object's keys sorted and no whitespace, so the
/// hash does not depend on the file's layout or on serde_json's map order.
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// [`seed`] from a thread that is not inside a Tokio runtime (the demo
/// binary's main thread once the host runs on its own).
pub fn seed_blocking() -> SeedReport {
    impress_service_core::runtime::block_on(seed())
}

/// The spec as it is stored: the file's JSON, and for the Notes surface its
/// `state.open` set to the root notes' record ids (see the module docs).
pub fn prepared_spec(name: &str, json: &str) -> Result<Value, String> {
    let mut spec: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    if name == NOTES_NAME {
        if let Ok(all) = notes::list() {
            let roots: Vec<Value> = all
                .iter()
                .filter(|n| n.parent.is_none())
                .map(|n| Value::String(n.record.to_string()))
                .collect();
            spec["state"]["open"] = Value::Array(roots);
        }
    }
    Ok(spec)
}

fn seed_notes(report: &mut SeedReport) {
    match notes::count() {
        Ok(0) => {}
        Ok(_) => return,
        Err(e) => {
            report
                .errors
                .push(format!("notes: {} ({})", e.message, e.code));
            return;
        }
    }
    for (key, parent, title, body) in SEED_NOTES {
        match notes::add(*parent, title, body, Some(key)) {
            Ok(_) => report.notes_seeded += 1,
            Err(e) => report
                .errors
                .push(format!("note {key}: {} ({})", e.message, e.code)),
        }
    }
}

async fn invoke(name: &str, args: Value) -> Result<Value, String> {
    let verb = VerbDescriptor::find(name).ok_or_else(|| format!("{name} is not linked"))?;
    pipeline::invoke(verb, Call::agent(TAG, args))
        .await
        .map_err(|e| e.to_string())
}

/// A stored surface tagged [`TAG`], as `surface_list` shows it.
#[derive(Debug, Clone)]
struct Stored {
    name: String,
    id: String,
    tags: Vec<String>,
}

/// Every stored surface tagged [`TAG`].
async fn list_tour_surfaces() -> Result<Vec<Stored>, String> {
    let answer = invoke(LIST, json!({})).await?;
    if answer["ok"] != json!(true) {
        return Err(answer["message"].as_str().unwrap_or("refused").to_string());
    }
    Ok(answer["surfaces"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let tags: Vec<String> = row["tags"]
                        .as_array()?
                        .iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect();
                    tags.iter().any(|t| t == TAG).then_some(())?;
                    Some(Stored {
                        name: row["name"].as_str()?.to_string(),
                        id: row["id"].as_str()?.to_string(),
                        tags,
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn validate(spec: &Value) -> Result<(), String> {
    let checked = invoke(VALIDATE, json!({ "spec": spec })).await?;
    if checked["ok"] != json!(true) {
        return Err(format!(
            "does not validate: {} {}",
            checked["message"].as_str().unwrap_or(""),
            checked["problems"]
        ));
    }
    Ok(())
}

/// Validate, then replace, a stored surface's spec (same id), then refresh
/// its content tag. `surface_update` takes no tags, so the tag is written to
/// the row's `tags` field in the process store, the one field of the
/// surface record seeding writes directly; every other field stays the
/// surface service's.
async fn update(name: &str, stored: &Stored, spec: Value, tag: &str) -> Result<(), String> {
    validate(&spec).await?;
    let updated = invoke(
        UPDATE,
        json!({ "id": stored.id, "spec": spec, "name": name }),
    )
    .await?;
    if updated["ok"] != json!(true) {
        return Err(format!(
            "not updated: {} ({})",
            updated["message"].as_str().unwrap_or("refused"),
            updated["code"].as_str().unwrap_or("?")
        ));
    }
    let mut tags: Vec<String> = stored
        .tags
        .iter()
        .filter(|t| !t.starts_with(SPEC_TAG_PREFIX))
        .cloned()
        .collect();
    tags.push(tag.to_string());
    write_tags(&stored.id, &tags)
}

fn write_tags(id: &str, tags: &[String]) -> Result<(), String> {
    use impress_item::{FieldMutation, Value as ItemValue};
    let record = id
        .parse::<impress_item::ItemId>()
        .map_err(|e| format!("surface id {id}: {e}"))?;
    let (store, fallback) = koine_store::process::acquire_or_stand_in();
    if fallback {
        return Err("no store is installed in this process; the tag was not written".into());
    }
    store
        .update(
            record,
            vec![FieldMutation::SetPayload(
                "tags".into(),
                ItemValue::Array(tags.iter().cloned().map(ItemValue::String).collect()),
            )],
        )
        .map_err(|e| format!("tags not written: {e}"))
}

/// Validate, then create, one surface; the new row's id.
async fn create(name: &str, spec: Value, tag: &str) -> Result<String, String> {
    validate(&spec).await?;
    let created = invoke(
        CREATE,
        json!({ "spec": spec, "name": name, "tags": [TAG, tag] }),
    )
    .await?;
    if created["ok"] != json!(true) {
        return Err(format!(
            "not created: {} ({})",
            created["message"].as_str().unwrap_or("refused"),
            created["code"].as_str().unwrap_or("?")
        ));
    }
    created["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "created, but the answer has no id".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_is_json_with_a_title_line_and_a_description() {
        for (name, json) in specs() {
            let spec: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(spec["surface"], "1.0", "{name}");
            assert_eq!(
                spec["name"], name,
                "the spec's name is what seeding matches on"
            );
            let column = spec["root"]["column"]
                .as_array()
                .unwrap_or_else(|| panic!("{name}"));
            // The page shows the surface's name as its title (koine-web's
            // surface page), so the first node is the description, never a
            // heading. Markdown renders on the web (A-D7), so backticks
            // around a verb's name are fine.
            let description = column[0]["text"].as_str().unwrap_or_default();
            assert!(
                description.len() > 20,
                "{name}: first node describes the surface"
            );
            assert!(
                !description.starts_with('#'),
                "{name}: the description is not a heading"
            );
        }
    }

    #[test]
    fn the_seed_notes_are_two_collections_of_notes_with_bodies() {
        let roots = SEED_NOTES.iter().filter(|(_, p, _, _)| p.is_none()).count();
        assert_eq!(roots, 2);
        assert_eq!(SEED_NOTES.len(), 6);
        for (key, parent, _, body) in SEED_NOTES {
            assert!(body.len() > 40, "{key} has a paragraph");
            if let Some(parent) = parent {
                let (_, grand, _, _) = SEED_NOTES
                    .iter()
                    .find(|(k, _, _, _)| k == parent)
                    .unwrap_or_else(|| panic!("{key}'s parent exists"));
                assert!(grand.is_none(), "{key} is a note in a collection");
            }
        }
    }

    #[test]
    fn the_spec_tag_is_the_canonical_jsons_fnv1a() {
        // The FNV-1a 64 reference values (Fowler/Noll/Vo test vectors).
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        // Layout and key order do not matter; content does.
        let a = spec_tag(r#"{"b": [1, {"y": 2, "x": 1}], "a": "t"}"#).unwrap();
        let b = spec_tag("{\"a\":\"t\",\n \"b\":[1,{\"x\":1,\"y\":2}]}").unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with(SPEC_TAG_PREFIX) && a.len() == SPEC_TAG_PREFIX.len() + 16);
        assert_ne!(
            a,
            spec_tag(r#"{"a": "u", "b": [1, {"x": 1, "y": 2}]}"#).unwrap()
        );
        // Every tour spec has its own.
        let tags: std::collections::BTreeSet<String> = all_specs()
            .iter()
            .map(|(_, j)| spec_tag(j).unwrap())
            .collect();
        assert_eq!(tags.len(), all_specs().len());
    }
}
