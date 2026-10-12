//! Wire types the surface verbs take and return.
//!
//! As `impress-layout-service/src/dto.rs`'s module docs put it: a crate type
//! that already derives `serde` **and** `schemars::JsonSchema` is used
//! **directly** on the trait — [`impress_surface::RenderTree`],
//! [`impress_surface::Event`]. What is here is only the shapes those crates
//! do not have: how a caller names a pane to show a surface in, a spec that
//! is still JSON, and the result envelopes.
//!
//! # The wire (version 1)
//!
//! Every result is snake_case and carries `"wire_version": 1`
//! (`impress_service_core::wire::WIRE_VERSION`); every refusal is `ok: false` with
//! a `code` and a `message` (`impress_service_core::refusal`). Every argument
//! an agent sends is strict: an unknown field is refused with
//! `invalid-argument` naming it, never ignored.

use std::collections::BTreeMap;

use impress_service_core::refusal::codes;
use impress_service_core::wire::{wire_version, WIRE_VERSION};
use impress_service_core::Refusal;
use impress_surface::{Problem, SurfaceSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// A surface spec as an argument: JSON, read by the verb rather than by the
/// argument parser, so a structural mistake comes back as a located problem
/// (`surface_validate`) or an `invalid-spec` refusal listing every problem
/// (`surface_create`, `surface_update`) — never a bare parse failure (review
/// AC-F13). Its schema is `SurfaceSpec`'s, inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpecArg(pub Value);

impl SpecArg {
    /// A spec this crate already holds, as the JSON an agent would send.
    pub fn of(spec: &SurfaceSpec) -> Self {
        SpecArg(serde_json::to_value(spec).unwrap_or(Value::Null))
    }
}

impl From<SurfaceSpec> for SpecArg {
    fn from(spec: SurfaceSpec) -> Self {
        SpecArg::of(&spec)
    }
}

impl schemars::JsonSchema for SpecArg {
    fn schema_name() -> String {
        "SurfaceSpec".to_string()
    }

    fn is_referenceable() -> bool {
        false
    }

    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        SurfaceSpec::json_schema(gen)
    }
}

/// Values for a surface's declared `params`, by name: a record id each
/// (what `{{param.<name>}}` and a query source's `$param` read). Given, they
/// are the whole binding for this call; absent, the params come from the pane
/// that shows the surface (see `docs/agent-surfaces.md`, "Params").
pub type ParamsArg = BTreeMap<String, String>;

/// Which pane `surface_show` should put the surface in: exactly ONE of the
/// suite's one pane-reference spelling — `{"id": N}`, `{"role": "detail"}`,
/// `{"direction": "right"}`, `{"focused": true}` (the layout verbs'
/// `PaneRefWire`, so a reference copied from any layout result works here) —
/// or `{"split": {"direction": "horizontal"|"vertical"}}` for a new pane
/// beside the focused one. None or several is refused with `invalid-argument`
/// naming them, and so is any other key (review AC-F3, RS-S18).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShowTargetDto {
    /// A tile id from a prior `get_layout` / `surface_show` result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// `navigator` | `list` | `detail` | `preview` | `console` | any role a
    /// preset assigned. The surface replaces whatever that pane shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The pane one step from the focused one: `left` | `right` | `up` |
    /// `down` | `next` | `prev`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<impress_layout::Direction>,
    /// `true`: the focused pane itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// Open a NEW pane beside the focused one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitTargetDto>,
}

/// What a [`ShowTargetDto`] names, once checked.
#[derive(Debug, Clone, PartialEq)]
pub enum ShowTarget {
    /// An existing pane, in the layout verbs' own spelling.
    Pane(impress_layout::PaneRefWire),
    Split {
        direction: String,
    },
}

impl ShowTargetDto {
    /// The one target this names, or why it names none or several.
    pub fn target(&self) -> Result<ShowTarget, Refusal> {
        let set: Vec<&str> = [
            ("id", self.id.is_some()),
            ("role", self.role.is_some()),
            ("direction", self.direction.is_some()),
            ("focused", self.focused.is_some()),
            ("split", self.split.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, on)| on.then_some(name))
        .collect();
        if set.len() != 1 {
            return Err(Refusal::invalid_argument(if set.is_empty() {
                "target names no pane: give exactly one of {\"id\": N}, {\"role\": \"…\"}, \
                 {\"direction\": \"…\"}, {\"focused\": true} or {\"split\": {\"direction\": \
                 \"horizontal\"|\"vertical\"}}"
                    .to_string()
            } else {
                format!(
                    "target sets {} — give exactly one of id, role, direction, focused or split",
                    set.join(" and ")
                )
            }));
        }
        let pane = impress_layout::PaneRefWire::default();
        Ok(match self {
            ShowTargetDto { id: Some(id), .. } => ShowTarget::Pane(impress_layout::PaneRefWire {
                id: Some(*id),
                ..pane
            }),
            ShowTargetDto {
                role: Some(role), ..
            } => {
                if role.trim().is_empty() {
                    return Err(Refusal::invalid_argument("target.role is empty"));
                }
                ShowTarget::Pane(impress_layout::PaneRefWire::role(role))
            }
            ShowTargetDto {
                direction: Some(d), ..
            } => ShowTarget::Pane(impress_layout::PaneRefWire::direction(*d)),
            ShowTargetDto {
                focused: Some(focused),
                ..
            } => {
                if !focused {
                    return Err(Refusal::invalid_argument(
                        "target.focused: false names no pane; say true, or name one",
                    ));
                }
                ShowTarget::Pane(impress_layout::PaneRefWire::focused())
            }
            ShowTargetDto {
                split: Some(split), ..
            } => {
                if !split.from_focused {
                    return Err(Refusal::invalid_argument(
                        "target.split.from_focused: false is not supported; a split is always \
                         of the focused pane (leave it out)",
                    ));
                }
                match split.direction.as_str() {
                    "horizontal" | "vertical" => ShowTarget::Split {
                        direction: split.direction.clone(),
                    },
                    other => {
                        return Err(Refusal::invalid_argument(format!(
                            "target.split.direction '{other}' is neither 'horizontal' (side by \
                             side) nor 'vertical' (stacked)"
                        )))
                    }
                }
            }
            _ => unreachable!("exactly one field is set"),
        })
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitTargetDto {
    /// `horizontal` (side by side) or `vertical` (stacked).
    pub direction: String,
    /// Split the focused pane — the only split this verb makes. `false` is
    /// refused rather than quietly treated as `true`.
    #[serde(default = "yes")]
    pub from_focused: bool,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// `surface_schema`'s answer: the JSON Schema plus a worked example and the
/// rules the schema cannot say, so an agent never has to read Rust source to
/// learn the vocabulary (ADR-0033 D8).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceSchemaResult {
    pub ok: bool,
    pub message: String,
    /// A JSON Schema (draft 7) a validator can check a spec against: every
    /// node kind, source and action is a real `oneOf` branch.
    pub schema: serde_json::Value,
    pub example: SurfaceSpec,
    /// The template language, widget ids and problem paths, in prose.
    pub rules: SurfaceRules,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// What `surface_schema` says in prose because a JSON Schema cannot.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceRules {
    /// `{{root.a.b}}` references: roots, path syntax, text vs reference.
    pub templates: String,
    /// How a widget's id is assigned, and why to give one.
    pub widget_ids: String,
    /// What a problem's `path` points at, and the two severities.
    pub problems: String,
    /// How `params` are bound.
    pub params: String,
}

/// `surface_validate`'s answer: every problem, named by path — the pure
/// checks plus this service's own (every named verb exists, and its literal
/// arguments fit its input schema). `ok` is true when no problem is an
/// `error`; warnings alone leave it true. Not ok is `code: "invalid-spec"`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceValidateResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub problems: Vec<Problem>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// The code a spec with error-severity problems is refused with.
pub const INVALID_SPEC: &str = "invalid-spec";

impl SurfaceValidateResult {
    pub fn of(problems: Vec<Problem>) -> Self {
        let errors = problems.iter().filter(|p| p.is_error()).count();
        let warnings = problems.len() - errors;
        let ok = errors == 0;
        Self {
            ok,
            message: format!("{errors} error(s), {warnings} warning(s)"),
            code: (!ok).then(|| INVALID_SPEC.to_string()),
            problems,
            wire_version: WIRE_VERSION,
        }
    }
}

/// One surface row, in full — `surface_get`, and what `surface_create` /
/// `surface_update` echo back.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The row's revision: 1 on create, +1 on every update. Pass it back as
    /// `surface_update`'s `expected_revision` to refuse a lost update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<SurfaceSpec>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// What validation found: on a refused create or update (`code:
    /// "invalid-spec"`) every problem, errors first; on a stored one, the
    /// warnings it was stored with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<Problem>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            id: None,
            name: None,
            version: None,
            revision: None,
            spec: None,
            tags: Vec::new(),
            created: None,
            modified: None,
            problems: Vec::new(),
            wire_version: WIRE_VERSION,
        }
    }

    /// A spec with error-severity problems, refused with every problem.
    pub fn invalid_spec(problems: Vec<Problem>) -> Self {
        let errors = problems.iter().filter(|p| p.is_error()).count();
        let first = problems
            .iter()
            .find(|p| p.is_error())
            .map(|p| {
                format!(
                    "{}: {}",
                    if p.path.is_empty() { "/" } else { &p.path },
                    p.message
                )
            })
            .unwrap_or_default();
        let mut refused = Self::refused(Refusal::new(
            INVALID_SPEC,
            format!("the spec has {errors} error(s), nothing was stored — first: {first}"),
        ));
        let mut problems = problems;
        problems.sort_by_key(|p| !p.is_error());
        refused.problems = problems;
        refused
    }

    pub fn from_row(row: &crate::store::SurfaceRow) -> Self {
        Self {
            ok: true,
            code: None,
            message: format!("surface '{}' ({})", row.name, row.id),
            id: Some(row.id.to_string()),
            name: Some(row.name.clone()),
            version: row.version.clone(),
            revision: Some(row.revision),
            spec: Some(row.spec.clone()),
            tags: row.tags.clone(),
            created: Some(row.created.to_rfc3339()),
            modified: Some(row.modified.to_rfc3339()),
            problems: Vec::new(),
            wire_version: WIRE_VERSION,
        }
    }
}

/// One row as `surface_list` shows it — no `spec` (an agent lists to pick an
/// id, not to re-download every document at once; `surface_get` has the
/// spec).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceSummaryDto {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// See [`SurfaceResult::revision`].
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created: String,
    pub modified: String,
}

impl From<&crate::store::SurfaceRow> for SurfaceSummaryDto {
    fn from(row: &crate::store::SurfaceRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name.clone(),
            version: row.version.clone(),
            revision: row.revision,
            tags: row.tags.clone(),
            created: row.created.to_rfc3339(),
            modified: row.modified.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceListResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub surfaces: Vec<SurfaceSummaryDto>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceDeleteResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

/// `surface_show`'s answer: which pane now shows the surface.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceShowResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile: Option<u64>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub affected_panes: Vec<u64>,
    /// The app and device whose layout now shows the surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceShowResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            tile: None,
            focused: false,
            affected_panes: Vec::new(),
            app_id: None,
            device: None,
            wire_version: WIRE_VERSION,
        }
    }
}

/// `surface_render`'s answer: the resolved tree, or a reason it could not be
/// built (a `surface_get`-style not-found, rather than a partial tree —
/// `impress_surface::resolve` itself never fails, so the only failure mode
/// here is "no such surface").
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceRenderResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<impress_surface::RenderTree>,
    /// Every source whose fetch failed on this render, with the reason. The
    /// tree is still returned — a failed source only empties the paths it
    /// fed, and the placeholders drawn for those paths carry the same
    /// message as their `reason`. This field is the machine-readable copy,
    /// for an agent calling `surface_render` directly rather than reading
    /// the rendered pane. Empty on a render where every source answered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_errors: Vec<SourceError>,
    #[serde(flatten)]
    pub revisions: Revisions,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceRenderResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            tree: None,
            source_errors: Vec::new(),
            revisions: Revisions::default(),
            wire_version: WIRE_VERSION,
        }
    }
}

/// What a render or dispatch was built from — so a pane can tell the feed's
/// echo of its own write from someone else's (review SK-K15), and an agent
/// can see which params a render used.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Revisions {
    /// The spec's `revision` (1 on create, +1 per update).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// The state row's revision: moves on every state write, by anyone.
    /// Absent while the instance runs on the spec's initial state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_revision: Option<u64>,
    /// The params this render resolved against, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

/// One source that did not answer, and why — the wire form of
/// [`crate::runtime::SurfaceRuntime`]'s `source_errors` map.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourceError {
    /// The source's name as the spec declares it.
    pub name: String,
    /// What the failed fetch said — a refusal from the verb, a transport
    /// error, or "imprint is not running".
    pub message: String,
}

/// `surface_state_get`'s answer.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceStateResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceStateResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            state: None,
            wire_version: WIRE_VERSION,
        }
    }
}

/// One effect the runtime carried out (or tried to) while dispatching an
/// event — named so a caller can tell "done" from "nobody was listening"
/// (root CLAUDE.md, ADR-0032) instead of a dispatch silently swallowing a
/// failed `call`/`publish`/`open`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EffectOutcomeDto {
    /// `call` | `publish` | `emit` | `open` | `refresh`.
    pub kind: String,
    pub ok: bool,
    pub message: String,
    /// Why this effect failed, machine-readable: `unknown-verb`,
    /// `verb-failed`, `no-pane`, a layout refusal's own code (`unknown-tile`,
    /// …), `store-error`. Absent when it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl EffectOutcomeDto {
    pub fn done(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            ok: true,
            message: message.into(),
            code: None,
        }
    }

    pub fn failed(kind: &str, refusal: Refusal) -> Self {
        Self {
            kind: kind.to_string(),
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
        }
    }
}

/// `surface_dispatch`'s answer: the re-rendered tree plus what each effect
/// did.
///
/// **When `ok` is true.** A dispatch is `ok` only when the event was reduced
/// AND every effect it produced succeeded. Three outcomes:
///
/// * the event could not be reduced (no such widget, a template that does not
///   resolve, no such surface): `ok: false`, the refusal's own `code`, no
///   `tree`, no `effects`, and nothing was written;
/// * the event was reduced and its state saved, but at least one effect
///   failed (a `call` the verb refused, a `publish` with no pane to publish
///   from): `ok: false`, `code: "effect-failed"`, `effects_failed` > 0, the
///   message naming each failure, and the re-rendered `tree` and every
///   per-effect outcome in `effects` — the state change stands, and the
///   caller can see exactly which effect did not happen (review RS-S12,
///   AC-F11);
/// * everything happened: `ok: true`.
///
/// A source that fails while re-rendering does not make a dispatch fail — the
/// event was handled — but it is listed in `source_errors`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceDispatchResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<impress_surface::RenderTree>,
    /// What each effect did, in the order the reducer produced them.
    #[serde(default)]
    pub effects: Vec<EffectOutcomeDto>,
    /// How many of `effects` failed. Non-zero means `ok` is false.
    #[serde(default)]
    pub effects_failed: u32,
    /// Sources that failed while re-rendering after the dispatch. See
    /// [`SurfaceRenderResult::source_errors`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_errors: Vec<SourceError>,
    #[serde(flatten)]
    pub revisions: Revisions,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceDispatchResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            tree: None,
            effects: Vec::new(),
            effects_failed: 0,
            source_errors: Vec::new(),
            revisions: Revisions::default(),
            wire_version: WIRE_VERSION,
        }
    }

    /// The answer for a dispatch that was reduced: `ok` only when every
    /// effect succeeded (see the type's docs).
    pub fn dispatched(
        tree: impress_surface::RenderTree,
        effects: Vec<EffectOutcomeDto>,
        source_errors: Vec<SourceError>,
        revisions: Revisions,
    ) -> Self {
        let failed: Vec<&EffectOutcomeDto> = effects.iter().filter(|e| !e.ok).collect();
        let (ok, code, message) = if failed.is_empty() {
            (
                true,
                None,
                format!("dispatched; {} effect(s)", effects.len()),
            )
        } else {
            let reasons: Vec<String> = failed
                .iter()
                .map(|e| format!("{}: {}", e.kind, e.message))
                .collect();
            (
                false,
                Some(codes::EFFECT_FAILED.to_string()),
                format!(
                    "dispatched; {} effect(s), {} failed — {}",
                    effects.len(),
                    failed.len(),
                    reasons.join("; ")
                ),
            )
        };
        let effects_failed = failed.len() as u32;
        Self {
            ok,
            message,
            code,
            tree: Some(tree),
            effects,
            effects_failed,
            source_errors,
            revisions,
            wire_version: WIRE_VERSION,
        }
    }
}

/// One row as `surface_events` / `surface_wait` show it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceEventDto {
    pub surface: String,
    pub host: String,
    pub seq: u64,
    pub name: String,
    pub payload: serde_json::Value,
    pub at: String,
    /// Who caused it: `human` for a click or edit in the pane, `agent` for a
    /// dispatch over MCP or HTTP (review SK-K5, AC-F5). An agent waiting on
    /// `surface_wait` tells the person's action from its own by this.
    pub actor: String,
}

impl From<&crate::store::EventRow> for SurfaceEventDto {
    fn from(row: &crate::store::EventRow) -> Self {
        Self {
            surface: row.surface.to_string(),
            host: row.host.clone(),
            seq: row.seq,
            name: row.name.clone(),
            payload: row.payload.clone(),
            at: row.at.to_rfc3339(),
            actor: crate::runtime::actor_name(row.actor).to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceEventsResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default)]
    pub events: Vec<SurfaceEventDto>,
    /// The cursor to pass as `after_seq` on the next call — the `seq` of the
    /// last event returned, or `after_seq` itself when none were.
    pub next_seq: u64,
    /// True when the ring (the last 200 events) was pruned past `after_seq`:
    /// events between it and the first one returned are gone.
    #[serde(default)]
    pub gap: bool,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceEventsResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            events: Vec::new(),
            next_seq: 0,
            gap: false,
            wire_version: WIRE_VERSION,
        }
    }
}

/// `surface_wait`'s answer: the same shape as [`SurfaceEventsResult`] plus
/// `timed_out`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceWaitResult {
    pub ok: bool,
    pub message: String,
    /// Why it was refused, machine-readable (`not-found`, `conflict`,
    /// `invalid-argument`, `store-error`, `effect-failed`, a reduce error's
    /// own code, …). Absent when `ok` is true. See
    /// `impress_service_core::refusal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default)]
    pub events: Vec<SurfaceEventDto>,
    pub next_seq: u64,
    pub timed_out: bool,
    /// See [`SurfaceEventsResult::gap`].
    #[serde(default)]
    pub gap: bool,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl SurfaceWaitResult {
    /// A refusal: `ok: false`, the refusal's `code` and `message`.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            message: refusal.message,
            code: Some(refusal.code),
            events: Vec::new(),
            next_seq: 0,
            timed_out: false,
            gap: false,
            wire_version: WIRE_VERSION,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceExamplesResult {
    pub ok: bool,
    pub message: String,
    pub examples: Vec<SurfaceSpec>,
    /// The wire version (1): see this module's docs.
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

#[cfg(test)]
mod audit_schema_tests {
    use super::*;
    use impress_surface::resolve::RenderKind;
    use impress_surface::{RenderNode, RenderTree};

    fn dispatch_with(kind: RenderKind) -> serde_json::Value {
        let tree = RenderTree {
            root: RenderNode {
                id: "root".into(),
                label: None,
                help: None,
                node: kind,
            },
            focus_order: vec!["star-paper".into()],
        };
        serde_json::to_value(SurfaceDispatchResult::dispatched(
            tree,
            vec![EffectOutcomeDto::done("call", "triaged")],
            Vec::new(),
            Revisions::default(),
        ))
        .unwrap()
    }

    #[test]
    fn real_dispatch_schema_accepts_finite_recursive_tree() {
        let raw = dispatch_with(RenderKind::Column {
            items: vec![RenderNode {
                id: "star-paper".into(),
                label: None,
                help: None,
                node: RenderKind::Button {
                    label: "Star".into(),
                },
            }],
        });
        let schema = serde_json::to_value(schemars::schema_for!(SurfaceDispatchResult)).unwrap();
        let (ids, truncated) = impress_service_core::pipeline::audit::result_ids(&raw, &schema);
        assert!(!truncated, "finite native dispatch was incomplete: {ids:?}");
        assert_eq!(
            ids.get("$.tree.root.node.items.0.id"),
            Some(&serde_json::json!("star-paper"))
        );
    }

    #[test]
    fn real_dispatch_schema_does_not_capture_untyped_table_row_ids() {
        let raw = dispatch_with(RenderKind::Table {
            rows: serde_json::json!([{"id": "private-row"}]),
            columns: vec!["title".into()],
        });
        let schema = serde_json::to_value(schemars::schema_for!(SurfaceDispatchResult)).unwrap();
        let (ids, truncated) = impress_service_core::pipeline::audit::result_ids(&raw, &schema);
        assert!(truncated);
        assert!(!ids.values().any(|value| value == "private-row"));
    }
}
