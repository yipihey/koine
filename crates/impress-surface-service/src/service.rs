//! `ImpressSurfaceService` — every ADR-0033 verb, as an `#[impress_method]`, over
//! [`crate::store::SurfaceStore`] and [`crate::runtime::SurfaceRuntime`].
//!
//! Modelled on `impress-layout-service/src/service.rs`: one trait, one
//! method per verb, so MCP, the CLI and impel's agent loop get all fifteen
//! together (ADR-0033 D8's five-verb loop plus the read/state/event half).
//!
//! # Strict arguments, versioned results
//!
//! Every verb's arguments are strict (`strict_args`): an unknown field is
//! refused with `invalid-argument` naming it. Every result carries
//! `wire_version` (see `crate::dto`). A spec is validated wherever it is
//! stored: `surface_create` and `surface_update` refuse a spec with
//! error-severity problems (`invalid-spec`, every problem listed) and store
//! one with warnings, listing them.
//!
//! # `host`: the state instance
//!
//! A surface's working state and its event ring are kept per `(surface,
//! host)`. `host` defaults to this device's id
//! ([`impress_layout_service::resolve_device`]) — the instance the app's
//! panes use — so every pane on one device that shows a surface shares its
//! state (they share one runtime too, through the store's one registry), and
//! an agent reaches that instance by leaving `host` out. Pass a `host` only
//! to drive a private instance no pane shows (a headless test, a dry run).
//! The layout verbs call the same value `device`.

use std::sync::Arc;

use impress_item::item::{ActorKind, ItemId};
use impress_item::store::AttributedStore;
use impress_layout_service::{resolve_device, DefaultLayoutService};
use impress_pane_query::{process_manifest, KindManifest};
use impress_service_core::async_trait;
use impress_service_core::pipeline::{self, Call};
use impress_service_core::refusal::codes;
use impress_service_core::wire::WIRE_VERSION;
use impress_service_core::{Refusal, VerbDescriptor};
use impress_service_macros::{impress_service, impress_service_impl};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use impress_surface::{
    example_paper_triage, example_signal_explorer, validate_json, walk_actions, Action, Event,
    Problem, Source, SurfaceSpec,
};
use serde_json::Value;

use crate::dto::{
    ParamsArg, ShowTargetDto, SpecArg, SurfaceDeleteResult, SurfaceDispatchResult, SurfaceEventDto,
    SurfaceEventsResult, SurfaceExamplesResult, SurfaceListResult, SurfaceRenderResult,
    SurfaceResult, SurfaceRules, SurfaceSchemaResult, SurfaceShowResult, SurfaceStateResult,
    SurfaceSummaryDto, SurfaceValidateResult, SurfaceWaitResult,
};
use crate::runtime::{
    pane_params_for, show_in_pane, surface_item_query, verb_exists, DefaultExecutor,
    SessionRegistry, VerbHost,
};
use crate::store::SurfaceStore;

/// How often `surface_wait` polls the event ring (ADR-0033 D6 gives the app's
/// invalidation feed the same period; this crate has no feed to hook, so it
/// polls the store directly at the same cadence).
const WAIT_POLL_MS: u64 = 250;

/// The longest `surface_wait` holds a call open (AC-F2). Longer than this
/// outlives the request timeout of the MCP clients and HTTP callers in use,
/// which then retry a call that is still running.
pub const MAX_WAIT_MS: u64 = 55_000;

/// Agent surfaces: create/validate/store a surface spec, show it in a pane,
/// render it headlessly, and drive it — `surface_render` is what an agent
/// calls to inspect its own GUI without a screenshot (ADR-0033 D2).
#[impress_service]
pub trait ImpressSurfaceService: Send + Sync + 'static {
    /// The surface spec's JSON Schema (a real one: a validator can check a
    /// spec against it), a worked example, and the rules a schema cannot
    /// say — templates, widget ids, problem paths, params — so an agent
    /// authoring in a chat never has to read Rust source (ADR-0033 D8).
    #[impress_method(replay = full)]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn surface_schema(&self) -> SurfaceSchemaResult;

    /// Every problem with a spec, by JSON pointer and severity: structure
    /// (a missing field, an unknown key, a node that is not one kind), the
    /// vocabulary's own rules, and whether every named verb exists and its
    /// literal arguments fit that verb's input schema. `ok` is false (code
    /// `invalid-spec`) when any problem is an error; warnings leave it true.
    #[impress_method(replay = full)]
    #[impress_example(
        name = "g3-validate",
        args = r#"{"spec":{"surface":"1.0","name":"G3 surface","state":{"message":"ready"},"root":{"column":[{"text":"{{state.message}}","id":"message"},{"button":{"label":"Set message","on_click":[{"set":{"path":"state.message","value":"done"}},{"emit":{"name":"changed","payload":{"message":"{{state.message}}"}}}]},"id":"set-message"}]}}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn surface_validate(&self, spec: SpecArg) -> SurfaceValidateResult;

    /// Store a spec as a new `impress/ui/surface@1.0.0` row, after
    /// validating it exactly as `surface_validate` does: a spec with an
    /// error is refused (`invalid-spec`, every problem in `problems`) and
    /// nothing is stored; warnings are stored and listed. `name` overrides
    /// the row's label (the spec's own `name` is untouched); `tags` are
    /// free-text labels shown by `surface_list`.
    #[impress_method(replay = full, safety = mutating, effects(reads = ["impress/ui/surface@1.0.0"], writes = ["impress/ui/surface@1.0.0"]))]
    #[impress_example(
        name = "g3-create",
        args = r#"{"spec":{"surface":"1.0","name":"G3 surface","state":{"message":"ready"},"root":{"column":[{"text":"{{state.message}}","id":"message"},{"button":{"label":"Set message","on_click":[{"set":{"path":"state.message","value":"done"}},{"emit":{"name":"changed","payload":{"message":"{{state.message}}"}}}]},"id":"set-message"}]}}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn surface_create(
        &self,
        spec: SpecArg,
        name: Option<String>,
        tags: Option<Vec<String>>,
    ) -> SurfaceResult;

    /// Replace a surface's spec (validated as by `surface_create`) and bump
    /// its `revision`. The row's `name` is kept unless `name` is given. Pass
    /// the `revision` you last read as `expected_revision` to be refused
    /// (`conflict`) instead of overwriting a change someone else made since.
    #[impress_method(replay = full, safety = mutating, effects(reads = ["impress/ui/surface@1.0.0"], writes = ["impress/ui/surface@1.0.0"]))]
    #[impress_example(
        name = "g3-update",
        args = r#"{"id":"66000000-0000-4000-8000-000000000003","spec":{"surface":"1.0","name":"G3 revised surface","state":{"message":"ready"},"root":{"column":[{"text":"{{state.message}}","id":"message"},{"button":{"label":"Set message","on_click":[{"set":{"path":"state.message","value":"done"}},{"emit":{"name":"changed","payload":{"message":"{{state.message}}"}}}]},"id":"set-message"}]}},"name":"G3 revised surface","expected_revision":1}"#,
        expect = r#"{"ok":true,"revision":2}"#
    )]
    async fn surface_update(
        &self,
        id: String,
        spec: SpecArg,
        name: Option<String>,
        expected_revision: Option<u64>,
    ) -> SurfaceResult;

    /// One surface row, spec included.
    #[impress_method(replay = full, effects(reads = ["impress/ui/surface@1.0.0"]))]
    #[impress_example(
        name = "g3-get",
        args = r#"{"id":"66000000-0000-4000-8000-000000000004"}"#,
        expect = r#"{"ok":true,"name":"G3 surface","revision":1}"#
    )]
    async fn surface_get(&self, id: String) -> SurfaceResult;

    /// Every stored surface, oldest first, without their specs (see
    /// `surface_get` for the full document).
    #[impress_method(replay = full, effects(reads = ["impress/ui/surface@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn surface_list(&self) -> SurfaceListResult;

    /// Delete a surface and every state/event row that belongs to it.
    #[impress_method(replay = full, safety = destructive, effects(reads = ["impress/ui/surface-event@1.0.0", "impress/ui/surface-state@1.0.0", "impress/ui/surface@1.0.0"], writes = ["impress/ui/surface@1.0.0", "impress/ui/surface-state@1.0.0", "impress/ui/surface-event@1.0.0"]))]
    #[impress_example(
        name = "g3-delete",
        args = r#"{"id":"66000000-0000-4000-8000-000000000005"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn surface_delete(&self, id: String) -> SurfaceDeleteResult;

    /// Put a surface in a pane of `app_id`'s window on `device` (this device
    /// when absent). `target` is exactly one of `{"tile": N}`, `{"role":
    /// "detail"}` or `{"split": {"direction": "horizontal"|"vertical"}}` (a
    /// new pane beside the focused one). Composes ordinary `layout-service`
    /// verbs — a surface pane is not a special case of the layout tree
    /// (ADR-0033 D1).
    #[impress_method(replay = full, safety = mutating, effects(reads = ["impress/ui/surface-state@1.0.0", "impress/ui/surface@1.0.0", "impress/ui/layout@1.0.0", "impress/ui/preset@1.0.0"], writes = ["impress/ui/layout@1.0.0"]))]
    #[impress_example(
        name = "g3-show",
        args = r#"{"id":"66000000-0000-4000-8000-000000000006","target":{"role":"detail"},"app_id":"impress","device":"g3-surface-show"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn surface_show(
        &self,
        id: String,
        target: ShowTargetDto,
        app_id: String,
        device: Option<String>,
    ) -> SurfaceShowResult;

    /// The resolved render tree of one `(surface, host)` instance — exactly
    /// what a renderer turns into pixels, so an agent inspects its own GUI
    /// headlessly (ADR-0033 D2). Runs every stale source first. `params`
    /// binds the surface's declared params for this call; without it they
    /// come from the pane that shows the surface.
    #[impress_method(replay = full, effects(reads = ["impress/ui/surface@1.0.0", "impress/ui/surface-state@1.0.0", any("evaluates the surface's verb sources")]))]
    #[impress_example(
        name = "g3-render",
        args = r#"{"id":"66000000-0000-4000-8000-000000000007","host":"g3-surface-examples"}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn surface_render(
        &self,
        id: String,
        host: Option<String>,
        params: Option<ParamsArg>,
    ) -> SurfaceRenderResult;

    /// The working state of one `(surface, host)` instance — the spec's own
    /// initial `state` block if nothing has been dispatched to it yet.
    #[impress_method(replay = full, effects(reads = ["impress/ui/surface@1.0.0", "impress/ui/surface-state@1.0.0"]))]
    #[impress_example(
        name = "g3-state-get",
        args = r#"{"id":"66000000-0000-4000-8000-000000000008","host":"g3-surface-examples"}"#,
        expect = r#"{"ok":true,"state":{"message":"ready"}}"#
    )]
    async fn surface_state_get(&self, id: String, host: Option<String>) -> SurfaceStateResult;

    /// Overwrite the working state of one `(surface, host)` instance
    /// directly (bypassing `reduce` — for seeding a surface's state, not
    /// for an ordinary field edit, which goes through `surface_dispatch`).
    #[impress_method(replay = full, safety = mutating, effects(reads = ["impress/ui/surface@1.0.0", "impress/ui/surface-state@1.0.0"], writes = ["impress/ui/surface-state@1.0.0"]))]
    #[impress_example(
        name = "g3-state-set",
        args = r#"{"id":"66000000-0000-4000-8000-000000000009","host":"g3-surface-examples","state":{"message":"revised"}}"#,
        expect = r#"{"ok":true,"state":{"message":"revised"}}"#
    )]
    async fn surface_state_set(
        &self,
        id: String,
        state: Value,
        host: Option<String>,
    ) -> SurfaceStateResult;

    /// Reduce one renderer event (`{"widget", "kind", "value"}`), persist
    /// the resulting state, run every effect it produced, and re-render —
    /// as the agent. `ok` only when every effect happened. `params` as for
    /// `surface_render`.
    #[impress_method(replay = full, safety = mutating, effects(reads = ["impress/ui/surface@1.0.0", "impress/ui/surface-state@1.0.0", any("evaluates the surface's verb sources")], writes = ["impress/ui/surface-state@1.0.0", "impress/ui/surface-event@1.0.0", any("runs the surface's actions")]))]
    #[impress_example(
        name = "g3-dispatch",
        args = r#"{"id":"66000000-0000-4000-8000-000000000010","host":"g3-surface-examples","event":{"widget":"set-message","kind":"click","value":null}}"#,
        expect = r#"{"ok":true}"#
    )]
    async fn surface_dispatch(
        &self,
        id: String,
        event: Event,
        host: Option<String>,
        params: Option<ParamsArg>,
    ) -> SurfaceDispatchResult;

    /// A page of `(surface, host)`'s emitted events with `seq > after_seq`
    /// (0, every event still in the ring, when absent).
    #[impress_method(replay = full, effects(reads = ["impress/ui/surface@1.0.0", "impress/ui/surface-event@1.0.0"]))]
    #[impress_example(
        name = "g3-events",
        args = r#"{"id":"66000000-0000-4000-8000-000000000011","host":"g3-surface-examples","after_seq":0}"#,
        expect = r#"{"ok":true,"events":[{"name":"changed","payload":{"message":"seeded"}}]}"#
    )]
    async fn surface_events(
        &self,
        id: String,
        after_seq: Option<u64>,
        host: Option<String>,
    ) -> SurfaceEventsResult;

    /// Long-poll for the next event past `after_seq`, up to `timeout_ms`
    /// (at most 55000). Returns as soon as one lands, or on timeout with
    /// `timed_out: true` and no events — the primitive the five-verb loop
    /// calls "wait" (ADR-0033 D5/D6).
    #[impress_method(replay = full, effects(reads = ["impress/ui/surface@1.0.0", "impress/ui/surface-event@1.0.0"]))]
    #[impress_example(
        name = "g3-wait",
        args = r#"{"id":"66000000-0000-4000-8000-000000000012","host":"g3-surface-examples","after_seq":0,"timeout_ms":100}"#,
        expect = r#"{"ok":true,"events":[{"name":"changed","payload":{"message":"seeded"}}],"timed_out":false}"#
    )]
    async fn surface_wait(
        &self,
        id: String,
        after_seq: Option<u64>,
        timeout_ms: u64,
        host: Option<String>,
    ) -> SurfaceWaitResult;

    /// Every worked example this build ships — the signal explorer first,
    /// then the paper-triage surface over the user's own unread papers — so
    /// an agent can start from a spec that already validates.
    #[impress_method(replay = full)]
    #[impress_example(name = "default", args = r#"{}"#)]
    async fn surface_examples(&self) -> SurfaceExamplesResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Store-backed `ImpressSurfaceService`. `new()` uses the process-wide store and
/// session registry, matching `impress-layout-service::DefaultLayoutService`;
/// `with_store` gives hermetic tests their own of each.
/// Run [`crate::rename::RenamePass`] against `store` once per process — see
/// `impress-layout-service::service`'s `ensure_renamed`, which this mirrors.
/// The shipped table
/// ([`impress_service_core::lifecycle::SHIPPED_RENAMES`]) is empty, so this
/// costs one branch and touches the store not at all until a real rename is
/// appended.
fn ensure_renamed(store: &Arc<dyn AttributedStore>) {
    use impress_service_core::lifecycle::SHIPPED_RENAMES;
    if SHIPPED_RENAMES.is_empty() {
        return;
    }
    static RAN: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    RAN.get_or_init(|| {
        if let Err(e) =
            crate::rename::RenamePass::new(store.clone()).run_if_needed(&SHIPPED_RENAMES)
        {
            tracing::error!(target: "surface", "rename pass: {e}");
        }
    });
}

#[derive(Clone)]
pub struct DefaultImpressSurfaceService {
    store: Option<Arc<dyn AttributedStore>>,
    sessions: Option<Arc<SessionRegistry>>,
    /// The record kinds this service's executors and layout service read in.
    /// Passed at construction; [`Self::new`] (the inventory's, which has no
    /// arguments) takes the [`process_manifest::current`].
    manifest: Arc<KindManifest>,
    /// See [`crate::runtime::VerbHost`]. Threaded into every [`DefaultExecutor`]
    /// this service builds ([`Self::executor`]) and consulted directly by
    /// [`Self::surface_validate`], so a spec naming a host-only verb both
    /// runs and validates clean once a host is installed.
    verb_host: Option<Arc<dyn VerbHost>>,
    /// The executor a host process built (its layout sessions, its verb host,
    /// its app) — `impress-store-ffi` passes the one its panes use, so an
    /// HTTP route runs exactly what a pane would. `None`: built per call.
    executor: Option<DefaultExecutor>,
}

impl Default for DefaultImpressSurfaceService {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultImpressSurfaceService {
    /// The process-wide store and session registry, with the
    /// [`process_manifest::current`].
    pub fn new() -> Self {
        Self {
            store: None,
            sessions: None,
            manifest: process_manifest::current(),
            verb_host: None,
            executor: None,
        }
    }

    /// An instance over an explicit store, with a private session registry
    /// reading the record kinds in `manifest`.
    pub fn with_store(
        store: Arc<dyn AttributedStore>,
        manifest: impl Into<Arc<KindManifest>>,
    ) -> Self {
        let manifest = manifest.into();
        Self {
            store: Some(store),
            sessions: Some(Arc::new(SessionRegistry::with_manifest(manifest.clone()))),
            manifest,
            ..Self::new()
        }
    }

    /// An instance over `store` whose runtimes live in `sessions` — the one
    /// registry every other caller on that store in this process uses
    /// (RS-S1). `impress-store-ffi` builds its service this way.
    pub fn with_store_and_sessions(
        store: Arc<dyn AttributedStore>,
        sessions: Arc<SessionRegistry>,
        manifest: impl Into<Arc<KindManifest>>,
    ) -> Self {
        Self {
            store: Some(store),
            sessions: Some(sessions),
            manifest: manifest.into(),
            ..Self::new()
        }
    }

    /// This instance, with the record kinds in `manifest` — for [`Self::new`],
    /// which owns the process store and has no manifest argument.
    pub fn with_manifest(mut self, manifest: impl Into<Arc<KindManifest>>) -> Self {
        self.manifest = manifest.into();
        self
    }

    /// Install a [`VerbHost`] — builder style, matching
    /// [`DefaultExecutor::with_verb_host`].
    pub fn with_verb_host(mut self, host: Arc<dyn VerbHost>) -> Self {
        self.verb_host = Some(host);
        self
    }

    /// Run every source, effect and `surface_show` through `executor` — see
    /// the field.
    pub fn with_executor(mut self, executor: DefaultExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    fn store_arc(&self) -> Arc<dyn AttributedStore> {
        self.store_arc_checked().0
    }

    /// [`Self::store_arc`], and whether it is the in-memory stand-in the
    /// store service substitutes for a store it could not open — asked of
    /// the concrete handle before it is widened to the port.
    fn store_arc_checked(&self) -> (Arc<dyn AttributedStore>, bool) {
        let (store, fallback): (Arc<dyn AttributedStore>, bool) = match &self.store {
            Some(store) => (store.clone(), false),
            None => koine_store::process::acquire_or_stand_in(),
        };
        ensure_renamed(&store);
        (store, fallback)
    }

    fn surfaces(&self) -> SurfaceStore {
        SurfaceStore::new(self.store_arc())
    }

    /// The surface store for a verb that writes: refused with
    /// `store-unavailable` when the store service handed out its in-memory
    /// stand-in, because a write there answers `ok` and vanishes (review
    /// AC-F20).
    fn surfaces_for_write(&self) -> std::result::Result<SurfaceStore, Refusal> {
        let (store, fallback) = self.store_arc_checked();
        if fallback {
            return Err(Refusal::store_unavailable(format!(
                "the store{} could not be opened, so this write would land in a temporary \
                 in-memory stand-in and vanish; nothing was written",
                match koine_store::process::intended_location() {
                    Some(at) => format!(" at {at}"),
                    None => String::new(),
                }
            )));
        }
        Ok(SurfaceStore::new(store))
    }

    fn registry(&self) -> Arc<SessionRegistry> {
        self.sessions
            .clone()
            .unwrap_or_else(SessionRegistry::shared)
    }

    fn executor(&self) -> DefaultExecutor {
        if let Some(executor) = &self.executor {
            return executor.clone();
        }
        let executor = match &self.store {
            Some(s) => DefaultExecutor::with_store(s.clone(), self.manifest.clone()),
            None => DefaultExecutor::new(self.store_arc(), self.manifest.clone()),
        };
        match &self.verb_host {
            Some(host) => executor.with_verb_host(host.clone()),
            None => executor,
        }
    }

    fn layout(&self) -> DefaultLayoutService {
        if let Some(executor) = &self.executor {
            return executor.layout().clone();
        }
        match &self.store {
            Some(s) => DefaultLayoutService::with_store(s.clone(), self.manifest.clone()),
            None => DefaultLayoutService::new().with_manifest(self.manifest.clone()),
        }
    }

    /// Whether a verb can run here: linked, or answered by the host.
    fn verb_known(&self, verb: &str) -> bool {
        verb_exists(verb)
            || self
                .verb_host
                .as_ref()
                .is_some_and(|host| host.has_verb(verb))
    }

    /// Everything wrong with `raw`: [`validate_json`]'s problems, plus the
    /// ones only this process can see — a verb that does not exist here, and
    /// literal arguments that do not fit the verb's own input schema (review
    /// AC-F14). A spec that parsed comes back too.
    pub fn problems_of(&self, raw: &Value) -> (Option<SurfaceSpec>, Vec<Problem>) {
        let (spec, mut problems) = validate_json(raw);
        if let Some(spec) = &spec {
            vocabulary_problems(spec, &mut problems);
            for (path, verb, args_at, args) in verb_refs(spec) {
                if !self.verb_known(verb) {
                    problems.push(Problem::error(path, format!("no such verb: {verb}")));
                } else {
                    check_verb_args(verb, args, &args_at, &mut problems);
                }
            }
        }
        (spec, problems)
    }
}

fn parse_id(id: &str) -> std::result::Result<ItemId, Refusal> {
    id.trim()
        .parse::<ItemId>()
        .map_err(|_| Refusal::invalid_argument(format!("'{id}' is not a surface id")))
}

fn no_surface(id: &str) -> Refusal {
    Refusal::not_found(format!("no surface {id}"))
}

/// Every verb a spec's sources or `call` actions name: `(pointer to the
/// verb, verb, pointer to its args, args)`. Walks the tree through
/// `impress_surface`'s own walker, the one `validate` uses (review RS-S21).
fn verb_refs(spec: &SurfaceSpec) -> Vec<(String, &str, String, &Value)> {
    let mut out = Vec::new();
    for (name, source) in &spec.sources {
        if let Source::Verb { verb, args } = source {
            out.push((
                format!("/sources/{name}/verb"),
                verb.as_str(),
                format!("/sources/{name}/args"),
                args,
            ));
        }
    }
    for (at, action) in walk_actions(&spec.root) {
        if let Action::Call { verb, args, .. } = action {
            out.push((
                format!("{at}/call/verb"),
                verb.as_str(),
                format!("{at}/call/args"),
                args,
            ));
        }
    }
    out
}

/// What only the layout's vocabulary can say about a spec (review RS-S8,
/// RL-L12's surface side):
///
/// * an `open` names a view kind the layout does not know — an error, as the
///   layout's own `set-view-kind` refuses it (`impress_layout::ViewKindId::KNOWN`);
/// * a `publish` in a spec that declares no param publishes under the
///   generic `item` kind, which no pane's param follows — a warning. The
///   layout takes a selection of any kind (a channel is keyed by kind); a
///   pane follows only the kind its param declares, so declare a param of the
///   kind you publish, or publish `{"kind": K, "ids": […]}` explicitly.
fn vocabulary_problems(spec: &SurfaceSpec, problems: &mut Vec<Problem>) {
    for (at, action) in walk_actions(&spec.root) {
        match action {
            Action::Open { view_kind, .. } => {
                let kind = impress_layout::ViewKindId::from(view_kind.clone());
                if !view_kind.trim().is_empty() && !kind.is_known() {
                    let known: Vec<String> = impress_layout::ViewKindId::KNOWN
                        .iter()
                        .map(|k| k.to_string())
                        .collect();
                    problems.push(Problem::error(
                        format!("{at}/open/view_kind"),
                        format!(
                            "no view kind '{view_kind}' (the layout's: {})",
                            known.join(", ")
                        ),
                    ));
                }
            }
            Action::Publish { .. } if spec.params.is_empty() => {
                problems.push(Problem::warning(
                    format!("{at}/publish"),
                    "this surface declares no param, so the selection is published under the \
                     generic `item` kind, which no pane follows; declare a param of the kind \
                     you publish (or publish {\"kind\": K, \"ids\": […]})",
                ));
            }
            _ => {}
        }
    }
}

/// Whether `value` holds a `{{…}}` reference anywhere (so its type is only
/// known once resolved).
fn holds_reference(value: &Value) -> bool {
    match value {
        Value::String(s) => !matches!(
            impress_surface::Template::parse(s),
            impress_surface::Template::Literal(_)
        ),
        Value::Array(items) => items.iter().any(holds_reference),
        Value::Object(map) => map.values().any(holds_reference),
        _ => false,
    }
}

fn json_type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Check a verb's literal arguments against its input schema: every
/// required argument present, no argument the verb does not take (an error
/// when the verb refuses unknown arguments, else a warning — it would be
/// ignored), and every literal value of the declared JSON type. An argument
/// holding a `{{…}}` reference only has to be present: its value is known at
/// run time. A verb only the host answers has no schema here and is skipped.
fn check_verb_args(verb: &str, args: &Value, at: &str, problems: &mut Vec<Problem>) {
    let Some(descriptor) = impress_service_core::call::find(verb) else {
        return;
    };
    let schema = descriptor.input_schema();
    let empty = serde_json::Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let strict = schema.get("additionalProperties") == Some(&Value::Bool(false));
    let takes = || {
        let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
        names.sort_unstable();
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    };
    let map = match args {
        Value::Object(map) => map,
        Value::Null => &empty,
        other if holds_reference(other) => return,
        other => {
            problems.push(Problem::error(
                at,
                format!(
                    "{verb}'s arguments must be an object, got {}",
                    match other {
                        Value::Array(_) => "an array",
                        Value::String(_) => "a string",
                        Value::Number(_) => "a number",
                        _ => "a scalar",
                    }
                ),
            ));
            return;
        }
    };
    for name in &required {
        if !map.contains_key(*name) {
            problems.push(Problem::error(
                format!("{at}/{name}"),
                format!("{verb} requires argument `{name}`"),
            ));
        }
    }
    for (name, value) in map {
        let here = format!("{at}/{name}");
        let Some(property) = properties.get(name) else {
            let message = format!("{verb} takes no argument `{name}` (it takes: {})", takes());
            problems.push(if strict {
                Problem::error(here, message)
            } else {
                Problem::warning(here, format!("{message}; it would be ignored"))
            });
            continue;
        };
        if holds_reference(value) {
            continue;
        }
        let expected: Vec<&str> = match property.get("type") {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
            _ => continue, // a `$ref`, an `anyOf`: not checked here
        };
        if !expected.iter().any(|t| json_type_matches(t, value)) {
            problems.push(Problem::error(
                here,
                format!(
                    "{verb}'s `{name}` is {}, got {value}",
                    expected.join(" or ")
                ),
            ));
        }
    }
}

/// Rustdoc intra-doc links (`[`X`]`, `[text](path)`) are noise to an agent
/// reading the schema: keep the text, drop the brackets and targets.
fn plain_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "description" {
                    if let Value::String(text) = v {
                        *text = strip_doc_links(text);
                    }
                } else {
                    plain_descriptions(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(plain_descriptions),
        _ => {}
    }
}

fn strip_doc_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|c| open + c) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[close + 1..];
        // `[text](target)`: drop the target.
        if rest.starts_with('(') {
            if let Some(end) = rest.find(')') {
                rest = &rest[end + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn rules() -> SurfaceRules {
    SurfaceRules {
        templates: "A string may hold references written {{root.path}}. The roots are state \
                    (the surface's working state), param (its declared params), source (each \
                    source's last value), event (the event being handled, inside an action) \
                    and item (the current element, inside an action with `each`). A path is \
                    dotted names; a number indexes an array ({{state.selected.0}}). A string \
                    that is exactly one reference becomes that JSON value; mixed text \
                    stringifies each reference. Anything else between double braces is text \
                    (LaTeX and Typst are safe); there are no operators, conditionals or loops."
            .to_string(),
        widget_ids: "Events name a widget by its `id`. A node without one gets a positional \
                     id (n0.2.1: the root's third child's second child) that changes when the \
                     spec changes, so give every field, button, table and list an id."
            .to_string(),
        problems: "A problem's `path` is a JSON pointer into the spec as written \
                   (/root/column/1/field/select/options; \"\" is the spec itself). An `error` \
                   is refused by surface_create and surface_update; a `warning` (an unknown \
                   widget kind, which renders as a placeholder; a widget with no id; \
                   {{a.b}} kept as text) is stored."
            .to_string(),
        params: "A surface's `params` are bound per render: from the call's `params` argument \
                 when given (then only those), else from the pane that shows the surface — \
                 each declared name takes the pane's binding of the same name, which the \
                 layout resolves from the pane's own params (a fixed id, or its channel's \
                 selection of the param's kind; layout-service_bind-param sets one). {{param.x}} \
                 and a query source's $x read them; an unbound one is a placeholder."
            .to_string(),
    }
}

#[async_trait::async_trait]
impl ImpressSurfaceService for DefaultImpressSurfaceService {
    async fn surface_schema(&self) -> SurfaceSchemaResult {
        let mut schema =
            serde_json::to_value(schemars::schema_for!(SurfaceSpec)).unwrap_or(Value::Null);
        plain_descriptions(&mut schema);
        SurfaceSchemaResult {
            ok: true,
            message: "the surface spec schema".to_string(),
            schema,
            example: example_signal_explorer(),
            rules: rules(),
            wire_version: WIRE_VERSION,
        }
    }

    async fn surface_validate(&self, spec: SpecArg) -> SurfaceValidateResult {
        SurfaceValidateResult::of(self.problems_of(&spec.0).1)
    }

    async fn surface_create(
        &self,
        spec: SpecArg,
        name: Option<String>,
        tags: Option<Vec<String>>,
    ) -> SurfaceResult {
        let (parsed, problems) = self.problems_of(&spec.0);
        let Some(parsed) = parsed.filter(|_| !problems.iter().any(Problem::is_error)) else {
            return SurfaceResult::invalid_spec(problems);
        };
        let tags = tags.unwrap_or_default();
        let surfaces = match self.surfaces_for_write() {
            Ok(surfaces) => surfaces,
            Err(e) => return SurfaceResult::refused(e),
        };
        match surfaces.create(&parsed, name.as_deref(), &tags, ActorKind::Agent) {
            Ok(row) => SurfaceResult {
                problems,
                ..SurfaceResult::from_row(&row)
            },
            Err(e) => SurfaceResult::refused(e),
        }
    }

    async fn surface_update(
        &self,
        id: String,
        spec: SpecArg,
        name: Option<String>,
        expected_revision: Option<u64>,
    ) -> SurfaceResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceResult::refused(e),
        };
        let (parsed, problems) = self.problems_of(&spec.0);
        let Some(parsed) = parsed.filter(|_| !problems.iter().any(Problem::is_error)) else {
            return SurfaceResult::invalid_spec(problems);
        };
        let surfaces = match self.surfaces_for_write() {
            Ok(surfaces) => surfaces,
            Err(e) => return SurfaceResult::refused(e),
        };
        // No registry forget: every runtime, in this process or another,
        // compares the stored spec on its next call and reloads it (RS-S1).
        match surfaces.update(
            surface_id,
            &parsed,
            name.as_deref(),
            expected_revision,
            ActorKind::Agent,
        ) {
            Ok(row) => SurfaceResult {
                problems,
                ..SurfaceResult::from_row(&row)
            },
            Err(e) => SurfaceResult::refused(e),
        }
    }

    async fn surface_get(&self, id: String) -> SurfaceResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceResult::refused(e),
        };
        match self.surfaces().get(surface_id) {
            Ok(Some(row)) => SurfaceResult::from_row(&row),
            Ok(None) => SurfaceResult::refused(no_surface(&id)),
            Err(e) => SurfaceResult::refused(e),
        }
    }

    async fn surface_list(&self) -> SurfaceListResult {
        match self.surfaces().list() {
            Ok(rows) => SurfaceListResult {
                ok: true,
                code: None,
                message: format!("{} surface(s)", rows.len()),
                surfaces: rows.iter().map(SurfaceSummaryDto::from).collect(),
                wire_version: WIRE_VERSION,
            },
            Err(e) => SurfaceListResult {
                ok: false,
                message: e.message,
                code: Some(e.code),
                surfaces: Vec::new(),
                wire_version: WIRE_VERSION,
            },
        }
    }

    async fn surface_delete(&self, id: String) -> SurfaceDeleteResult {
        let refused = |e: Refusal| SurfaceDeleteResult {
            ok: false,
            message: e.message,
            code: Some(e.code),
            wire_version: WIRE_VERSION,
        };
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return refused(e),
        };
        let surfaces = match self.surfaces_for_write() {
            Ok(surfaces) => surfaces,
            Err(e) => return refused(e),
        };
        match surfaces.delete(surface_id) {
            Ok(true) => {
                self.registry().forget_surface(surface_id);
                SurfaceDeleteResult {
                    ok: true,
                    message: format!("deleted surface {id}"),
                    code: None,
                    wire_version: WIRE_VERSION,
                }
            }
            Ok(false) => refused(no_surface(&id)),
            Err(e) => refused(e),
        }
    }

    async fn surface_show(
        &self,
        id: String,
        target: ShowTargetDto,
        app_id: String,
        device: Option<String>,
    ) -> SurfaceShowResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceShowResult::refused(e),
        };
        let target = match target.target() {
            Ok(target) => target,
            Err(e) => return SurfaceShowResult::refused(e),
        };
        let app_id = app_id.trim().to_string();
        if app_id.is_empty() {
            return SurfaceShowResult::refused(Refusal::invalid_argument(
                "app_id is empty: name the app whose window should show the surface",
            ));
        }
        // A store read error is reported as what it is, never as "no
        // surface" (review AC-F19).
        let row = match self.surfaces_for_write().and_then(|s| s.get(surface_id)) {
            Ok(Some(row)) => row,
            Ok(None) => return SurfaceShowResult::refused(no_surface(&id)),
            Err(e) => return SurfaceShowResult::refused(e),
        };
        let device = resolve_device(device.as_deref());
        let layout = self.layout();
        let query = surface_item_query(surface_id);

        match show_in_pane(
            &layout,
            &app_id,
            Some(device.clone()),
            query,
            "surface",
            &target,
            Some(pane_params_for(&row.spec, &self.manifest)),
            Some("agent".to_string()),
        )
        .await
        {
            Ok((tile, focused, affected)) => {
                // `host` defaults to the device (this module's docs), so the
                // instance `surface_render`/`surface_dispatch` reach with no
                // `host` argument is the one this call just showed.
                let registry = self.registry();
                let surfaces = self.surfaces();
                let pane = crate::runtime::PaneHandle {
                    app_id: app_id.clone(),
                    device: device.clone(),
                    tile,
                };
                let _ = registry
                    .with(&surfaces, surface_id, &device, move |runtime| {
                        runtime.pane = Some(pane.clone());
                        Box::pin(async move { Ok::<(), Refusal>(()) })
                    })
                    .await;
                SurfaceShowResult {
                    ok: true,
                    code: None,
                    message: format!("surface shown in {app_id}'s tile {tile}"),
                    tile: Some(tile),
                    focused,
                    affected_panes: affected,
                    app_id: Some(app_id),
                    device: Some(device),
                    wire_version: WIRE_VERSION,
                }
            }
            Err(e) => SurfaceShowResult::refused(e),
        }
    }

    async fn surface_render(
        &self,
        id: String,
        host: Option<String>,
        params: Option<ParamsArg>,
    ) -> SurfaceRenderResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceRenderResult::refused(e),
        };
        let host = resolve_device(host.as_deref());
        let surfaces = self.surfaces();
        let registry = self.registry();
        let executor = self.executor();
        let outcome = registry
            .with(&surfaces, surface_id, &host, move |runtime| {
                Box::pin(async move {
                    runtime.bind_params(&executor, params.as_ref()).await?;
                    let tree = runtime.render(&executor).await;
                    Ok((tree, runtime.source_error_list(), runtime.revisions()))
                })
            })
            .await;
        match outcome {
            Ok((tree, source_errors, revisions)) => SurfaceRenderResult {
                ok: true,
                code: None,
                message: "rendered".to_string(),
                tree: Some(tree),
                source_errors,
                revisions,
                wire_version: WIRE_VERSION,
            },
            Err(e) => SurfaceRenderResult::refused(e),
        }
    }

    async fn surface_state_get(&self, id: String, host: Option<String>) -> SurfaceStateResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceStateResult::refused(e),
        };
        let host = resolve_device(host.as_deref());
        let surfaces = self.surfaces();
        let found = |message: &str, state: Value| SurfaceStateResult {
            ok: true,
            code: None,
            message: message.to_string(),
            state: Some(state),
            wire_version: WIRE_VERSION,
        };
        match surfaces.get_state(surface_id, &host) {
            Ok(Some(state)) => found("state", state),
            Ok(None) => match surfaces.get(surface_id) {
                Ok(Some(row)) => found(
                    "no dispatch yet — the spec's own initial state",
                    row.spec.state,
                ),
                Ok(None) => SurfaceStateResult::refused(no_surface(&id)),
                Err(e) => SurfaceStateResult::refused(e),
            },
            Err(e) => SurfaceStateResult::refused(e),
        }
    }

    async fn surface_state_set(
        &self,
        id: String,
        state: Value,
        host: Option<String>,
    ) -> SurfaceStateResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceStateResult::refused(e),
        };
        if !state.is_object() {
            return SurfaceStateResult::refused(Refusal::invalid_argument(
                "state must be a JSON object",
            ));
        }
        let host = resolve_device(host.as_deref());
        let surfaces = match self.surfaces_for_write() {
            Ok(surfaces) => surfaces,
            Err(e) => return SurfaceStateResult::refused(e),
        };
        let written = state.clone();
        let writer = surfaces.clone();
        // Through the registry, so a dispatch already running on this
        // instance finishes first and this write is not overwritten by its
        // stale copy of the state.
        let outcome = self
            .registry()
            .with(&surfaces, surface_id, &host, move |runtime| {
                Box::pin(async move { runtime.set_state(&writer, written, ActorKind::Agent) })
            })
            .await;
        match outcome {
            Ok(()) => SurfaceStateResult {
                ok: true,
                code: None,
                message: "state set".to_string(),
                state: Some(state),
                wire_version: WIRE_VERSION,
            },
            Err(e) => SurfaceStateResult::refused(e),
        }
    }

    async fn surface_dispatch(
        &self,
        id: String,
        event: Event,
        host: Option<String>,
        params: Option<ParamsArg>,
    ) -> SurfaceDispatchResult {
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceDispatchResult::refused(e),
        };
        let host = resolve_device(host.as_deref());
        let surfaces = match self.surfaces_for_write() {
            Ok(surfaces) => surfaces,
            Err(e) => return SurfaceDispatchResult::refused(e),
        };
        let registry = self.registry();
        let executor = self.executor();
        let writer = surfaces.clone();
        let outcome = registry
            .with(&surfaces, surface_id, &host, move |runtime| {
                Box::pin(async move {
                    runtime.bind_params(&executor, params.as_ref()).await?;
                    let (tree, effects) = runtime
                        .dispatch(&executor, &writer, &event, ActorKind::Agent)
                        .await?;
                    Ok((
                        tree,
                        effects,
                        runtime.source_error_list(),
                        runtime.revisions(),
                    ))
                })
            })
            .await;
        match outcome {
            Ok((tree, effects, source_errors, revisions)) => {
                SurfaceDispatchResult::dispatched(tree, effects, source_errors, revisions)
            }
            Err(e) => SurfaceDispatchResult::refused(e),
        }
    }

    async fn surface_events(
        &self,
        id: String,
        after_seq: Option<u64>,
        host: Option<String>,
    ) -> SurfaceEventsResult {
        let after_seq = after_seq.unwrap_or(0);
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceEventsResult::refused(e),
        };
        let host = resolve_device(host.as_deref());
        let surfaces = self.surfaces();
        match surfaces.get(surface_id) {
            Ok(Some(_)) => {}
            Ok(None) => return SurfaceEventsResult::refused(no_surface(&id)),
            Err(e) => return SurfaceEventsResult::refused(e),
        }
        match surfaces.events_after(surface_id, &host, after_seq, 0) {
            Ok(rows) => {
                let (next_seq, gap) = cursor_after(&rows, after_seq);
                SurfaceEventsResult {
                    ok: true,
                    code: None,
                    message: events_message(rows.len(), gap),
                    events: rows.iter().map(SurfaceEventDto::from).collect(),
                    next_seq,
                    gap,
                    wire_version: WIRE_VERSION,
                }
            }
            Err(e) => SurfaceEventsResult::refused(e),
        }
    }

    async fn surface_wait(
        &self,
        id: String,
        after_seq: Option<u64>,
        timeout_ms: u64,
        host: Option<String>,
    ) -> SurfaceWaitResult {
        let after_seq = after_seq.unwrap_or(0);
        let surface_id = match parse_id(&id) {
            Ok(id) => id,
            Err(e) => return SurfaceWaitResult::refused(e),
        };
        let host = resolve_device(host.as_deref());
        let surfaces = self.surfaces();
        match surfaces.get(surface_id) {
            Ok(Some(_)) => {}
            Ok(None) => return SurfaceWaitResult::refused(no_surface(&id)),
            Err(e) => return SurfaceWaitResult::refused(e),
        }

        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(timeout_ms.min(MAX_WAIT_MS));
        loop {
            match surfaces.events_after(surface_id, &host, after_seq, 0) {
                Ok(rows) if !rows.is_empty() => {
                    let (next_seq, gap) = cursor_after(&rows, after_seq);
                    return SurfaceWaitResult {
                        ok: true,
                        code: None,
                        message: events_message(rows.len(), gap),
                        events: rows.iter().map(SurfaceEventDto::from).collect(),
                        next_seq,
                        timed_out: false,
                        gap,
                        wire_version: WIRE_VERSION,
                    };
                }
                Ok(_) => {}
                Err(e) => return SurfaceWaitResult::refused(e),
            }
            if std::time::Instant::now() >= deadline {
                // Nothing past the cursor: the cursor stands. Never a second
                // read for it — an event landing between two reads would be
                // counted and never returned (AC-F2).
                return SurfaceWaitResult {
                    ok: true,
                    code: None,
                    message: "timed out; nothing new".to_string(),
                    events: Vec::new(),
                    next_seq: after_seq,
                    timed_out: true,
                    gap: false,
                    wire_version: WIRE_VERSION,
                };
            }
            tokio::time::sleep(std::time::Duration::from_millis(WAIT_POLL_MS)).await;
        }
    }

    async fn surface_examples(&self) -> SurfaceExamplesResult {
        SurfaceExamplesResult {
            ok: true,
            message: "2 example(s)".to_string(),
            examples: vec![example_signal_explorer(), example_paper_triage()],
            wire_version: WIRE_VERSION,
        }
    }
}

/// The cursor to hand back after reading `rows` past `after_seq`, and whether
/// the ring was pruned past the caller's cursor (events between it and the
/// first row returned are gone). Taken from the rows read — never from a
/// second read, which could count an event it did not return (AC-F2). `seq`
/// is gap-free (`SurfaceStore::append_event`), so a jump is pruning.
pub fn cursor_after(rows: &[crate::store::EventRow], after_seq: u64) -> (u64, bool) {
    match (rows.first(), rows.last()) {
        (Some(first), Some(last)) => (last.seq, after_seq > 0 && first.seq > after_seq + 1),
        _ => (after_seq, false),
    }
}

fn events_message(count: usize, gap: bool) -> String {
    if gap {
        format!("{count} event(s); older events past your cursor were pruned from the ring")
    } else {
        format!("{count} event(s)")
    }
}

fn impress_surface_service_instance() -> Arc<dyn ImpressSurfaceService> {
    Arc::new(DefaultImpressSurfaceService::new())
}

impress_service_impl! {
    service = ImpressSurfaceService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [],
        writes: [],
        reach: [],
    },
    impl = DefaultImpressSurfaceService,
    instance = || impress_surface_service_instance(),
    strict_args = true,
    methods = [
        surface_schema() -> SurfaceSchemaResult,
        surface_validate(
            /// The spec, as JSON.
            spec: SpecArg
        ) -> SurfaceValidateResult,
        surface_create(
            /// The spec, as JSON. Validated first; refused with every problem
            /// if it has an error.
            spec: SpecArg,
            /// The row's label; the spec's own `name` when absent.
            name: Option<String>,
            /// Free-text labels.
            tags: Option<Vec<String>>
        ) -> SurfaceResult,
        surface_update(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// The new spec, as JSON. Validated first.
            spec: SpecArg,
            /// Replacement row label; omit to keep the existing label.
            name: Option<String>,
            /// The `revision` you last read; refused with `conflict` if the
            /// row moved since.
            expected_revision: Option<u64>
        ) -> SurfaceResult,
        surface_get(
            /// UUID of the stored surface row returned by surface-create or surface-list.
            id: String
        ) -> SurfaceResult,
        surface_list() -> SurfaceListResult,
        surface_delete(
            /// UUID of the stored surface to delete together with its working state and events.
            id: String
        ) -> SurfaceDeleteResult,
        surface_show(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// Exactly one of {"tile": N}, {"role": "detail"}, or
            /// {"split": {"direction": "horizontal"|"vertical"}}.
            target: ShowTargetDto,
            /// The app whose window shows the surface: impress, imbib,
            /// imprint, implore, impel, impart.
            app_id: String,
            /// The device whose layout; this one when absent.
            device: Option<String>
        ) -> SurfaceShowResult,
        surface_render(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// The state instance; this device's (the app's panes') when
            /// absent.
            host: Option<String>,
            /// Values for the surface's declared params, by name (a record
            /// id each); from the showing pane when absent.
            params: Option<ParamsArg>
        ) -> SurfaceRenderResult,
        surface_state_get(
            /// UUID of the stored surface whose working state to read.
            id: String,
            /// State instance identifier; defaults to this device.
            host: Option<String>
        ) -> SurfaceStateResult,
        surface_state_set(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// The whole new state, a JSON object.
            state: Value,
            /// State instance identifier; defaults to this device.
            host: Option<String>
        ) -> SurfaceStateResult,
        surface_dispatch(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// {"widget": id, "kind": "change"|"click"|"select"|"submit",
            /// "value": json}.
            event: Event,
            /// State instance identifier; defaults to this device.
            host: Option<String>,
            /// Surface parameter bindings by name; defaults to those of the showing pane.
            params: Option<ParamsArg>
        ) -> SurfaceDispatchResult,
        surface_events(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// Return events with a larger `seq`; 0 (all) when absent.
            after_seq: Option<u64>,
            /// State instance identifier; defaults to this device.
            host: Option<String>
        ) -> SurfaceEventsResult,
        surface_wait(
            /// UUID of the stored surface returned by surface-create or surface-list.
            id: String,
            /// Wait for an event with a larger `seq`; 0 when absent.
            after_seq: Option<u64>,
            /// At most 55000.
            timeout_ms: u64,
            /// State instance identifier; defaults to this device.
            host: Option<String>
        ) -> SurfaceWaitResult,
        surface_examples() -> SurfaceExamplesResult,
    ],
}

// ---------------------------------------------------------------------------
// One verb, by name, from JSON arguments
// ---------------------------------------------------------------------------

/// Run one surface verb on `service` from its JSON arguments, exactly as the
/// MCP tool and the CLI subcommand of that name do: the arguments are parsed
/// into the SAME strict args struct `impress_service_impl!` generated for the
/// tool (so an unknown field is refused identically, naming it), and the
/// answer is the verb's own result, serialized unchanged. The HTTP mirror in
/// `impress-store-ffi` calls this, so an HTTP body is an MCP result by
/// construction (review AC-F9, RS-S13). `method` is the trait method's name
/// (`surface_show`); an unknown one is `None`.
pub async fn call_verb_on(
    service: &DefaultImpressSurfaceService,
    method: &str,
    args: Value,
) -> Option<Value> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    // The pipeline the MCP tool of this name runs (plan-verb-pipeline P2:
    // this mirror was one of the two bypasses): the same strict check
    // against the published input schema, nested fields included, the same
    // policy, span and audit — with THIS service instance as the handler
    // step, so the FFI's verb host, executor and app reach the verb.
    let tool = format!("impress-surface-service_{}", method.replace('_', "-"));
    let descriptor = VerbDescriptor::find(&tool)?;
    let call = Call::agent("http", args);
    macro_rules! verb {
        ($tool:literal, $args:ident, |$a:ident| $call:expr) => {{
            let answer = pipeline::invoke_with(descriptor, call, |args| async move {
                let $a: $args = match serde_json::from_value(args) {
                    Ok(parsed) => parsed,
                    Err(e) => return Ok(impress_service_core::strict::parse_refusal(&tool, e)),
                };
                let out = $call.await;
                Ok(serde_json::to_value(out).unwrap_or_else(|e| {
                    serde_json::to_value(SurfaceResult::refused(Refusal::internal(format!(
                        "encode {}: {e}",
                        $tool
                    ))))
                    .unwrap_or(Value::Null)
                }))
            })
            .await;
            Some(answer.unwrap_or_else(|e| {
                serde_json::to_value(SurfaceResult::refused(Refusal::new(
                    codes::VERB_FAILED,
                    e.to_string(),
                )))
                .unwrap_or(Value::Null)
            }))
        }};
    }
    match method {
        "surface_schema" => verb!(
            "surface-schema",
            __Impress_ImpressSurfaceService_surface_schema_Args,
            |_a| service.surface_schema()
        ),
        "surface_validate" => verb!(
            "surface-validate",
            __Impress_ImpressSurfaceService_surface_validate_Args,
            |a| service.surface_validate(a.spec)
        ),
        "surface_create" => verb!(
            "surface-create",
            __Impress_ImpressSurfaceService_surface_create_Args,
            |a| service.surface_create(a.spec, a.name, a.tags)
        ),
        "surface_update" => verb!(
            "surface-update",
            __Impress_ImpressSurfaceService_surface_update_Args,
            |a| service.surface_update(a.id, a.spec, a.name, a.expected_revision)
        ),
        "surface_get" => verb!(
            "surface-get",
            __Impress_ImpressSurfaceService_surface_get_Args,
            |a| service.surface_get(a.id)
        ),
        "surface_list" => verb!(
            "surface-list",
            __Impress_ImpressSurfaceService_surface_list_Args,
            |_a| service.surface_list()
        ),
        "surface_delete" => verb!(
            "surface-delete",
            __Impress_ImpressSurfaceService_surface_delete_Args,
            |a| service.surface_delete(a.id)
        ),
        "surface_show" => verb!(
            "surface-show",
            __Impress_ImpressSurfaceService_surface_show_Args,
            |a| service.surface_show(a.id, a.target, a.app_id, a.device)
        ),
        "surface_render" => verb!(
            "surface-render",
            __Impress_ImpressSurfaceService_surface_render_Args,
            |a| service.surface_render(a.id, a.host, a.params)
        ),
        "surface_state_get" => verb!(
            "surface-state-get",
            __Impress_ImpressSurfaceService_surface_state_get_Args,
            |a| service.surface_state_get(a.id, a.host)
        ),
        "surface_state_set" => verb!(
            "surface-state-set",
            __Impress_ImpressSurfaceService_surface_state_set_Args,
            |a| service.surface_state_set(a.id, a.state, a.host)
        ),
        "surface_dispatch" => verb!(
            "surface-dispatch",
            __Impress_ImpressSurfaceService_surface_dispatch_Args,
            |a| service.surface_dispatch(a.id, a.event, a.host, a.params)
        ),
        "surface_events" => verb!(
            "surface-events",
            __Impress_ImpressSurfaceService_surface_events_Args,
            |a| service.surface_events(a.id, a.after_seq, a.host)
        ),
        "surface_wait" => verb!(
            "surface-wait",
            __Impress_ImpressSurfaceService_surface_wait_Args,
            |a| service.surface_wait(a.id, a.after_seq, a.timeout_ms, a.host)
        ),
        "surface_examples" => verb!(
            "surface-examples",
            __Impress_ImpressSurfaceService_surface_examples_Args,
            |_a| service.surface_examples()
        ),
        _ => None,
    }
}
