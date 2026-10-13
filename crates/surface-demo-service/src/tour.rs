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
//! | Notes | an outline you rename, move, delete, add to | `note` records; `note-add/rename/move/delete` |
//! | Map of this app | a graph of verbs, kinds and surfaces | `inventory` |
//! | Concierge | an agent in the loop (`crate::concierge`) | `surface_wait` → react → `surface_create` → `surface_show` |
//!
//! The Notes spec is the one spec seeding touches before storing: its
//! `state.open` (the tree's expanded ids) is set to the record ids of the
//! root notes, so the outline opens unfolded. The file keeps `[]`, because
//! the ids are only known once the notes exist.

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

/// The verbs seeding calls, by their inventory names.
const LIST: &str = "impress-surface-service_surface-list";
const VALIDATE: &str = "impress-surface-service_surface-validate";
const CREATE: &str = "impress-surface-service_surface-create";

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

/// The notes seeded when the store has none: `(key, parent key, title)`.
/// Two roots with two children each.
pub const SEED_NOTES: &[(&str, Option<&str>, &str)] = &[
    ("research", None, "Research"),
    ("ideas", Some("research"), "Ideas"),
    ("experiments", Some("research"), "Experiments"),
    ("reading", None, "Reading"),
    ("papers", Some("reading"), "Papers to read"),
    ("koine", Some("reading"), "Notes on koine"),
];

/// What [`seed`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SeedReport {
    /// `(name, surface id)` of each surface created on this call.
    pub created: Vec<(String, String)>,
    /// `(name, surface id)` of each surface that was already there.
    pub existing: Vec<(String, String)>,
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

    /// The id of the surface named `name`, created or existing.
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.created
            .iter()
            .chain(self.existing.iter())
            .find(|(n, _)| n == name)
            .map(|(_, id)| id.as_str())
    }
}

/// Seed the tour: the notes when there are none, then every surface of
/// [`all_specs`] that is not already stored under [`TAG`] with its name.
/// Idempotent: a second call creates nothing. Never panics; what failed is
/// in the report's `errors`.
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
        if let Some(id) = existing.iter().find(|(n, _)| n == name).map(|(_, id)| id) {
            report.existing.push((name.to_string(), id.clone()));
            continue;
        }
        let spec = match prepared_spec(name, json) {
            Ok(spec) => spec,
            Err(e) => {
                report.errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        match create(name, spec).await {
            Ok(id) => report.created.push((name.to_string(), id)),
            Err(e) => report.errors.push(format!("{name}: {e}")),
        }
    }
    report
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
    for (key, parent, title) in SEED_NOTES {
        match notes::add(*parent, title, Some(key)) {
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

/// `(name, id)` of every stored surface tagged [`TAG`].
async fn list_tour_surfaces() -> Result<Vec<(String, String)>, String> {
    let answer = invoke(LIST, json!({})).await?;
    if answer["ok"] != json!(true) {
        return Err(answer["message"].as_str().unwrap_or("refused").to_string());
    }
    Ok(answer["surfaces"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| {
                    row["tags"]
                        .as_array()
                        .is_some_and(|tags| tags.iter().any(|t| t == TAG))
                })
                .filter_map(|row| {
                    Some((
                        row["name"].as_str()?.to_string(),
                        row["id"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Validate, then create, one surface; the new row's id.
async fn create(name: &str, spec: Value) -> Result<String, String> {
    let checked = invoke(VALIDATE, json!({ "spec": spec })).await?;
    if checked["ok"] != json!(true) {
        return Err(format!(
            "does not validate: {} {}",
            checked["message"].as_str().unwrap_or(""),
            checked["problems"]
        ));
    }
    let created = invoke(CREATE, json!({ "spec": spec, "name": name, "tags": [TAG] })).await?;
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
            let title = column[0]["text"].as_str().unwrap_or_default();
            assert!(title.starts_with("# "), "{name}: first node is the title");
            let description = column[1]["text"].as_str().unwrap_or_default();
            assert!(
                description.len() > 20,
                "{name}: second node describes the surface"
            );
        }
    }

    #[test]
    fn the_seed_notes_are_a_small_tree_with_two_roots() {
        let roots = SEED_NOTES.iter().filter(|(_, p, _)| p.is_none()).count();
        assert_eq!(roots, 2);
        assert_eq!(SEED_NOTES.len(), 6);
        for (key, parent, _) in SEED_NOTES {
            if let Some(parent) = parent {
                assert!(
                    SEED_NOTES.iter().any(|(k, _, _)| k == parent),
                    "{key}'s parent exists"
                );
            }
        }
    }
}
