//! The impure half of a surface: running its sources through the linked
//! `#[impress_service]` inventory and the store, and turning what the human
//! did into effects that actually happen (ADR-0033 D2/D4).
//!
//! [`impress_surface`] is pure — `plan`/`resolve`/`reduce` never touch a
//! store or call a verb themselves. This module is the other half: an
//! [`Executor`] that CAN, a [`SurfaceRuntime`] that holds one surface
//! instance's live state (spec, state, params, source cache) between calls,
//! and a [`SessionRegistry`] that holds one of those per `(surface, host)` —
//! the same shape `impress-layout-service/src/session.rs` gives the layout
//! tree, for the same reason: a process-wide table, one entry per scope,
//! created on first touch.
//!
//! # Where the linked inventory is reached
//!
//! `impress-capabilities` is the one crate meant to link every
//! `#[impress_service]` trait — but this crate cannot depend on it (nor on
//! `impress-capabilities`): both already depend on
//! `impress-surface-service` itself, and a dependency back would be a cycle.
//! [`call_verb`] therefore runs through `impress_service_core::call`, the one
//! copy of "find the descriptor, run its handler" both of those re-export
//! (review RS-S21), and only maps its error onto a [`Refusal`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use impress_item::item::{ActorKind, ItemId};
use impress_item::store::AttributedStore;
use impress_layout::{
    ChannelId, LinearDir, PaneRef, PaneSpec, ParamBinding, ParamSource, TileId, Verb, ViewKindId,
};
use impress_layout_service::dto::PaneRefDto as LayoutPaneRefDto;
use impress_layout_service::{DefaultLayoutService, LayoutService};
use impress_pane_query::{
    process_manifest, Bindings, ItemRef, KindManifest, PaneQueryError, Scope,
};
use impress_service_core::call;
use impress_service_core::descriptor_handle::VerbHandle;
use impress_service_core::pipeline::{self, Call, PipelineError};
use impress_surface::{
    plan, reduce, resolve_with_source_errors, state_path, CachedSource, Effect, Event, PaneQuery,
    ParamDecl, RenderTree, Source, SourceCache, SourceRequestKind, SurfaceSpec,
};
use serde_json::Value;

use crate::dto::{EffectOutcomeDto, ParamsArg, Revisions, ShowTarget};
use crate::store::{SurfaceRow, SurfaceStore};
use impress_service_core::refusal::codes;
use impress_service_core::Refusal;

/// See [`crate::Result`]: a stable `code` and a sentence.
pub type Result<T> = crate::Result<T>;

/// A layout verb's refusal, carried into a surface's own result with the
/// code the layout service gave it.
fn layout_refused(code: Option<String>, message: String) -> Refusal {
    Refusal::new(code.unwrap_or_else(|| "refused".to_string()), message)
}

// ---------------------------------------------------------------------------
// The linked inventory
// ---------------------------------------------------------------------------

/// Run one verb by its MCP tool name through the pipeline. Retain the exact
/// resolved handle across the call so a runtime provider's structured
/// refusal can be distinguished from a linked verb's ordinary result.
pub(crate) async fn call_verb(name: &str, args: Value) -> Result<Value> {
    // A surface's sources and effects run inside the surface verb's own call
    // when one encloses them, and the pipeline then inherits its caller and
    // trace (H-P2-1); with nothing enclosing, the surface runtime is itself
    // the agent.
    let caller = impress_service_core::pipeline::CallerIdentity::agent("surface");
    let handle = call::find(name)
        .ok_or_else(|| Refusal::new("unknown-verb", format!("Unknown tool: {name}")))?;
    let call = Call::new(caller, args);
    let call = if handle.name() == name {
        call
    } else {
        call.with_requested_name(name)
    };
    let value = pipeline::invoke_handle(handle.clone(), call)
        .await
        .map_err(|e| match e {
            PipelineError::Unavailable { .. } => {
                Refusal::new(codes::HOST_UNAVAILABLE, e.to_string())
            }
            PipelineError::Handler(_) => {
                Refusal::new(codes::VERB_FAILED, format!("{}: {e}", handle.name()))
            }
        })?;
    provider_result(handle, value)
}

fn provider_result(handle: VerbHandle, value: Value) -> Result<Value> {
    if matches!(handle, VerbHandle::Provider(_)) && value.get("ok") == Some(&Value::Bool(false)) {
        if let (Some(code), Some(message)) = (
            value.get("code").and_then(Value::as_str),
            value.get("message").and_then(Value::as_str),
        ) {
            return Err(Refusal::new(code, message));
        }
    }
    Ok(value)
}

/// Whether a verb name is in the linked inventory — what `surface_validate`
/// checks every `source`/`action` verb reference against (ADR-0033 D4: "a
/// source or action that names a verb calls it through the
/// `#[impress_service]` inventory in the host process").
pub fn verb_exists(name: &str) -> bool {
    call::find(name).is_some()
}

// ---------------------------------------------------------------------------
// VerbHost (ADR-0033 D4, amended 2026-09-23 for wave 5)
// ---------------------------------------------------------------------------

/// A second inventory the host process supplies for verbs this binary did
/// not link (ADR-0033 D4, amended 2026-09-23). Consulted only after the
/// linked inventory, so a linked verb is never shadowed.
///
/// In the app, `impress-store-ffi` implements this over `impel-tools`'
/// `call_tool` — the suite's already-linked full inventory, which reaches
/// imbib and imprint through their own HTTP routers and refuses, by name,
/// when the owning app is not running (see `impel_tools::ToolError`). A
/// domain core is therefore still linked once per app process, never twice
/// (ADR-0033 D4's own prohibition), and a verb whose app is down fails in
/// the source rather than silently writing the store behind the running
/// app's back.
///
/// Synchronous on purpose: the UniFFI callback into Swift is synchronous,
/// and the host's own `call_tool` blocks on its own Tokio runtime
/// (`impress_service_core::runtime::block_on`) rather than yielding to this
/// crate's. [`DefaultExecutor`] therefore runs a host call under
/// `tokio::task::spawn_blocking` (see its `call_verb`), so a slow host (an
/// HTTP round trip to imbib) never parks one of this runtime's own worker
/// threads.
pub trait VerbHost: Send + Sync {
    /// Whether the host can answer this verb at all — checked before
    /// [`Self::call_verb`], so `surface_validate` can report "no such verb"
    /// without actually calling it.
    fn has_verb(&self, name: &str) -> bool;
    /// Run the verb and return its JSON result.
    fn call_verb(&self, name: &str, args: Value) -> Result<Value>;
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Which pane, on which device, is currently showing a `(surface, host)`
/// instance — set by `surface_show`, consumed by the `publish`/`open`
/// effects. `None` until `surface_show` has run at least once for this
/// instance: a surface rendered headlessly (Tier A, `surface_render` with no
/// pane behind it) has nothing to publish a selection ON, and a `publish` or
/// `open` action against it fails cleanly rather than guessing a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneHandle {
    pub app_id: String,
    pub device: String,
    pub tile: u64,
}

/// What runs a surface's sources and effects. [`DefaultExecutor`] is the
/// only implementation the suite ships; the trait exists so Tier A tests can
/// swap in a fixture that never touches the inventory or the store.
#[async_trait::async_trait]
pub trait Executor: Send + Sync {
    /// Call a linked verb by name.
    async fn call_verb(&self, name: &str, args: Value) -> Result<Value>;
    /// Run a pane query — the store read every list pane uses, compiled the
    /// same way `impress_store_ffi::layout::compile_pane_query` does. Rows
    /// come back as a JSON array (each item's own JSON shape), never a
    /// domain-specific projection.
    ///
    /// `decls` are the surface's own `params`, so a `required` one that is
    /// unbound is refused by the compiler rather than read as "no filter".
    async fn run_query(
        &self,
        query: &PaneQuery,
        decls: &[ParamDecl],
        bindings: &Bindings,
    ) -> Result<Value>;
    /// Apply one gesture's layout verbs in `pane`'s layout (its app and
    /// device): all of them or none, as ONE layout step with one undo entry
    /// (`impress_layout_service::DefaultLayoutService::apply_verbs_as`,
    /// review PH-M2).
    ///
    /// The `publish` and `open` effects of a dispatch are compiled into
    /// these verbs by the runtime ([`SurfaceRuntime::dispatch`] says which
    /// effects share a gesture), so one click that publishes and opens is
    /// one ⌘Z. A refusal is the layout's own, code and message.
    ///
    /// `actor` is whoever caused the dispatch — the human clicking in the
    /// pane, or an agent — and is the actor the layout verbs record, so a
    /// person's click lands on the person's undo ring (review AC-F5).
    async fn apply_layout(
        &self,
        pane: &PaneHandle,
        verbs: Vec<Verb>,
        actor: ActorKind,
    ) -> Result<()>;
    /// Append an event row, attributed to `actor`, and return its `seq`.
    async fn emit(
        &self,
        surface: ItemId,
        host: &str,
        name: &str,
        payload: Value,
        actor: ActorKind,
    ) -> Result<u64>;
    /// Which pane, if any, is showing `surface` right now — recovered from
    /// the LAYOUT rather than remembered. A runtime instance only knows its
    /// pane if `surface_show` (or the FFI's `bind_pane`) ran on THIS
    /// instance, and instances are per `SharedSurface` handle: the pane view
    /// in the app has one, an HTTP dispatch on the same surface opens
    /// another, and the second answered every `publish`/`open` with "no pane
    /// shows this surface yet" while the pane sat on screen (verified live,
    /// 2026-09-23). The tree is the one source that cannot disagree with the
    /// window, so it is asked. Default `None`: a fixture that never shows a
    /// surface has nothing to find.
    async fn pane_showing(&self, _surface: ItemId) -> Option<PaneHandle> {
        None
    }
    /// Whether `pane` still shows `surface` — asked before a remembered
    /// pane is used for an effect (RS-S25): the pane may have been closed,
    /// or its tile given another query, since it was recorded. Default
    /// `true`: a fixture has no layout to disagree with.
    async fn pane_shows(&self, _pane: &PaneHandle, _surface: ItemId) -> bool {
        true
    }
    /// The pane's resolved parameter bindings (name → record id), as the
    /// layout computes them from the pane's `params` — what a surface's own
    /// `params` are bound from when no caller passes them (review RS-S3,
    /// AC-F15). Default `None`: a fixture has no layout.
    async fn pane_bindings(&self, _pane: &PaneHandle) -> Option<BTreeMap<String, String>> {
        None
    }
}

/// Lift each row's `payload` object to the top of the row.
///
/// A store `Item` serializes as its ENVELOPE — `id`, `created`, `modified`,
/// `parent`, `is_read`, … — with the domain fields nested under `payload`. A
/// surface author writes `{"table": {"columns": ["title", "year"], "rows":
/// "{{source.papers}}"}}`, which is what the S1 golden's rows look like
/// (`{"title": …, "year": …}`): flat. Rendered against the real executor the
/// table drew its header and two empty rows, because `row["title"]` was
/// `null` — the title sat at `row["payload"]["title"]` (verified live in the
/// impress window, 2026-09-22).
///
/// The envelope WINS on a name collision, and `payload` is left in place: a
/// spec that already reads `{{source.papers.0.payload.title}}` keeps working,
/// and no envelope field can be shadowed by a domain field that happens to
/// share its name (`id` is the case that matters — selection publishes it).
fn flatten_payloads(rows: Value) -> Value {
    let Value::Array(items) = rows else {
        return rows;
    };
    Value::Array(
        items
            .into_iter()
            .map(|item| {
                let Value::Object(envelope) = item else {
                    return item;
                };
                let Some(Value::Object(payload)) = envelope.get("payload") else {
                    return Value::Object(envelope);
                };
                let mut flat = payload.clone();
                for (key, value) in envelope {
                    flat.insert(key, value);
                }
                Value::Object(flat)
            })
            .collect(),
    )
}

/// The real [`Executor`]: verbs through the linked inventory, queries and
/// events through the store, publish/open through `impress-layout-service`.
#[derive(Clone)]
pub struct DefaultExecutor {
    store: Arc<dyn AttributedStore>,
    surfaces: SurfaceStore,
    layout: DefaultLayoutService,
    /// The record kinds `query` sources compile against and a published
    /// kind is read in (`as_layout_kind`). Passed at construction.
    manifest: Arc<KindManifest>,
    /// The second inventory consulted when a verb is not in this process's
    /// linked `#[impress_service]` set — see [`VerbHost`]. `None` in every
    /// binary that has not installed one (the CLI, MCP, and every Tier A
    /// test unless it opts in with [`Self::with_verb_host`]).
    verb_host: Option<Arc<dyn VerbHost>>,
    /// The app this executor serves, when it serves one (the app's own
    /// `SharedSurface` handle): its layout is searched first for the pane
    /// that shows a surface. Never a default — without it every app's live
    /// layout on this device is searched (review RS-S4, AC-F6).
    app_id: Option<String>,
}

impl DefaultExecutor {
    pub fn new(store: Arc<dyn AttributedStore>, manifest: impl Into<Arc<KindManifest>>) -> Self {
        let manifest = manifest.into();
        Self {
            surfaces: SurfaceStore::new(store.clone()),
            // `DefaultLayoutService::new()` reaches the SAME process-wide
            // store and session registry every other layout verb does —
            // never a private one — so a surface shown here is the surface
            // the GUI (or the CLI, or another MCP call) already has open.
            layout: DefaultLayoutService::new().with_manifest(manifest.clone()),
            manifest,
            store,
            verb_host: None,
            app_id: None,
        }
    }

    /// An instance over an explicit store — hermetic tests only, mirroring
    /// `impress-layout-service::DefaultLayoutService::with_store`.
    pub fn with_store(
        store: Arc<dyn AttributedStore>,
        manifest: impl Into<Arc<KindManifest>>,
    ) -> Self {
        let manifest = manifest.into();
        Self {
            surfaces: SurfaceStore::new(store.clone()),
            layout: DefaultLayoutService::with_store(store.clone(), manifest.clone()),
            manifest,
            store,
            verb_host: None,
            app_id: None,
        }
    }

    /// An instance whose layout verbs run in `sessions` — the registry the
    /// renderer in this process reads from — so an `open`/`publish` effect
    /// changes the tree the window is showing, not a private copy of it.
    pub fn with_store_and_sessions(
        store: Arc<dyn AttributedStore>,
        sessions: Arc<impress_layout_service::SessionRegistry>,
        manifest: impl Into<Arc<KindManifest>>,
    ) -> Self {
        let manifest = manifest.into();
        Self {
            surfaces: SurfaceStore::new(store.clone()),
            layout: DefaultLayoutService::with_store_and_sessions(
                store.clone(),
                sessions,
                manifest.clone(),
            ),
            manifest,
            store,
            verb_host: None,
            app_id: None,
        }
    }

    /// Install a [`VerbHost`] this executor consults for any verb its own
    /// linked inventory lacks (ADR-0033 D4, amended 2026-09-23). Builder
    /// style so `DefaultExecutor::new(store, manifest).with_verb_host(host)` reads as
    /// one setup step, matching every other `with_*` on this type.
    pub fn with_verb_host(mut self, host: Arc<dyn VerbHost>) -> Self {
        self.verb_host = Some(host);
        self
    }

    /// The app this executor serves — see the field.
    pub fn with_app(mut self, app_id: impl Into<String>) -> Self {
        let app_id = app_id.into();
        self.app_id = (!app_id.trim().is_empty()).then_some(app_id);
        self
    }

    /// The layout service this executor's `publish`/`open` run through —
    /// `surface_show` uses the same one, so a surface shown from this
    /// process lands in the tree its window draws.
    pub fn layout(&self) -> &DefaultLayoutService {
        &self.layout
    }

    /// Every app with a live layout row on `device`, this executor's own app
    /// first. Read from the store — no layout row is created by asking.
    fn apps_with_live_layout(&self, device: &str) -> Vec<String> {
        use impress_item::item::Value as ItemValue;
        use impress_item::query::{ItemQuery, Predicate};
        let query = ItemQuery {
            schema: Some(impress_item::schema::refs::IMPRESS_UI_LAYOUT),
            predicates: vec![
                Predicate::Eq(
                    impress_layout_service::store::field::DEVICE.into(),
                    ItemValue::String(device.to_string()),
                ),
                Predicate::Eq(
                    impress_layout_service::store::field::IS_LIVE.into(),
                    ItemValue::Bool(true),
                ),
            ],
            include_tags: false,
            include_references: false,
            assume_schema_rare: true,
            ..Default::default()
        };
        let mut apps: Vec<String> = self.app_id.iter().cloned().collect();
        match self.store.query(&query) {
            Ok(items) => {
                for item in items {
                    if let Some(ItemValue::String(app)) = item
                        .payload
                        .get(impress_layout_service::store::field::APP_ID)
                    {
                        if !apps.contains(app) {
                            apps.push(app.clone());
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(target: "surface", "list live layouts on {device}: {e}"),
        }
        apps
    }
}

#[async_trait::async_trait]
impl Executor for DefaultExecutor {
    async fn call_verb(&self, name: &str, args: Value) -> Result<Value> {
        if verb_exists(name) {
            // The inventory wins on a shared name: a verb linked into this
            // process is never shadowed by a host that happens to answer
            // for the same name too (ADR-0033's amendment is explicit about
            // this ordering).
            return call_verb(name, args).await;
        }
        let Some(host) = self.verb_host.clone() else {
            return Err(Refusal::new(
                "unknown-verb",
                format!(
                    "Unknown tool: {name} (not in this process's inventory; no verb host \
                     installed)"
                ),
            ));
        };
        if !host.has_verb(name) {
            return Err(Refusal::new(
                "unknown-verb",
                format!(
                    "Unknown tool: {name} (not in this process's inventory; host has no such \
                     verb)"
                ),
            ));
        }
        // Synchronous by design (see `VerbHost`'s docs): run it off this
        // runtime's own worker threads so a slow host call — an HTTP round
        // trip to imbib or imprint — cannot park one.
        let owned_name = name.to_string();
        // Tokio task locals do not cross spawn_blocking. Surface dispatch
        // and its linked effects already share one pipeline call; re-enter
        // its context just around this callback so a host bridge can carry
        // that trusted caller/trace/parent across its own boundary too.
        let parent_context = pipeline::context::current();
        tokio::task::spawn_blocking(move || match parent_context {
            Some(context) => {
                pipeline::context::sync_scope(context, || host.call_verb(&owned_name, args))
            }
            None => host.call_verb(&owned_name, args),
        })
        .await
        .map_err(|e| Refusal::internal(format!("verb host call to {name}: task panicked: {e}")))?
    }

    async fn run_query(
        &self,
        query: &PaneQuery,
        decls: &[ParamDecl],
        bindings: &Bindings,
    ) -> Result<Value> {
        let compiled = impress_pane_query_compile::compile(query, decls, bindings, &self.manifest)
            .map_err(|e: PaneQueryError| Refusal::new("query-refused", e.to_string()))?;
        let items = self
            .store
            .query(&compiled.item_query)
            .map_err(|e| Refusal::store(format!("run query: {e}")))?;
        let rows = serde_json::to_value(items)
            .map_err(|e| Refusal::internal(format!("encode query result: {e}")))?;
        Ok(flatten_payloads(rows))
    }

    async fn apply_layout(
        &self,
        pane: &PaneHandle,
        mut verbs: Vec<Verb>,
        actor: ActorKind,
    ) -> Result<()> {
        let device = Some(pane.device.clone());
        // A gesture of one verb goes through the one-verb path, so a lone
        // `publish` or `open` is logged and recorded exactly as before.
        let result = if verbs.len() == 1 {
            let verb = verbs.remove(0);
            self.layout
                .apply_verb_as(&pane.app_id, device, actor, None, move |_| Ok(verb))
        } else {
            self.layout
                .apply_verbs_as(&pane.app_id, device, actor, None, verbs)
        };
        if result.ok {
            Ok(())
        } else {
            Err(layout_refused(result.code, result.message))
        }
    }

    async fn emit(
        &self,
        surface: ItemId,
        host: &str,
        name: &str,
        payload: Value,
        actor: ActorKind,
    ) -> Result<u64> {
        self.surfaces
            .append_event(surface, host, name, &payload, actor)
    }

    async fn pane_showing(&self, surface: ItemId) -> Option<PaneHandle> {
        // The layout is device-scoped: this machine's trees, the ones its
        // windows draw. Which APP's tree is not assumed (review RS-S4,
        // AC-F6): this executor's own app first, then every app with a live
        // layout here.
        let device = impress_layout_service::resolve_device(None);
        let wanted = surface_item_query(surface);
        let layouts = impress_layout_service::store::LayoutStore::new(self.store.clone());
        for app_id in self.apps_with_live_layout(&device) {
            let Ok(Some((_, layout))) = layouts.live_row(&app_id, &device) else {
                continue;
            };
            let tile = layout.panes().into_iter().find(|tile| {
                layout.pane(*tile).is_some_and(|spec| {
                    spec.view_kind == ViewKindId::from(SURFACE_VIEW_KIND.to_string())
                        && spec.query == wanted
                })
            });
            if let Some(tile) = tile {
                return Some(PaneHandle {
                    app_id,
                    device,
                    tile: tile.raw(),
                });
            }
        }
        None
    }

    async fn pane_shows(&self, pane: &PaneHandle, surface: ItemId) -> bool {
        let result = self
            .layout
            .get_layout(pane.app_id.clone(), Some(pane.device.clone()))
            .await;
        let Some(layout) = result.layout else {
            return false;
        };
        let wanted = surface_item_query(surface);
        layout
            .pane(impress_layout::TileId::new(pane.tile))
            .is_some_and(|spec| {
                spec.view_kind == ViewKindId::from(SURFACE_VIEW_KIND.to_string())
                    && spec.query == wanted
            })
    }

    async fn pane_bindings(&self, pane: &PaneHandle) -> Option<BTreeMap<String, String>> {
        let result = self
            .layout
            .get_pane(
                pane.app_id.clone(),
                Some(pane.device.clone()),
                LayoutPaneRefDto::tile(impress_layout::TileId::new(pane.tile)),
            )
            .await;
        result.ok.then_some(result.bindings)
    }
}

/// `impress_layout::ViewKindId::SURFACE`'s spelling.
const SURFACE_VIEW_KIND: &str = "surface";

// ---------------------------------------------------------------------------
// Composing layout verbs — `surface_show` (service.rs), and the verbs a
// `publish`/`open` effect compiles to
// ---------------------------------------------------------------------------

/// Put a query+view-kind in the pane [`ShowTargetDto`] names, creating it
/// (a split) when the target says so. Returns `(tile, focused, affected_panes)`
/// — the same trio every layout verb answers with.
///
/// `surface_show`'s path. An `{"open": …}` action means the same thing but
/// is compiled to verbs by [`open_verbs`] instead, so it can share one step
/// with the rest of its click; the two must agree on what "put this in a
/// pane" means (`docs/agent-surfaces.md`'s "`open`" row).
///
/// `params`, when given, become the pane's parameters: the pane then
/// resolves one binding per name, which is what a surface's own params are
/// bound from ([`SurfaceRuntime::bind_params`]). A binding the pane already
/// has under the same name keeps its source (someone re-bound it); a new one
/// follows the window's default channel. `None` leaves the pane's params as
/// they are (an `open` action).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn show_in_pane(
    layout: &DefaultLayoutService,
    app_id: &str,
    device: Option<String>,
    query: PaneQuery,
    view_kind: &str,
    target: &ShowTarget,
    params: Option<Vec<ParamBinding>>,
    actor: Option<String>,
) -> Result<(u64, bool, Vec<u64>)> {
    let pane_ref = match target {
        ShowTarget::Pane(pane) => pane.clone(),
        ShowTarget::Split { direction } => {
            let mut new_pane = PaneSpec::new(query, ViewKindId::from(view_kind.to_string()));
            new_pane.params = params.unwrap_or_default();
            let split = layout
                .split(
                    app_id.to_string(),
                    device,
                    LayoutPaneRefDto::focused(),
                    direction.clone(),
                    true,
                    Some(new_pane),
                    actor,
                    None,
                )
                .await;
            if !split.ok {
                return Err(layout_refused(split.code, split.message));
            }
            let tile = split
                .focused
                .ok_or_else(|| Refusal::internal("split produced no focused tile"))?;
            return Ok((tile, true, split.affected_panes));
        }
    };
    // A tile or a role: read the pane, then replace what it shows in ONE
    // verb (one undo step), keeping its role, channel and session.
    let current = layout
        .get_pane(app_id.to_string(), device.clone(), pane_ref.clone())
        .await;
    if !current.ok {
        return Err(layout_refused(current.code, current.message));
    }
    let (Some(tile), Some(mut spec)) = (current.tile, current.spec) else {
        return Err(Refusal::new(
            "unknown-tile",
            format!("{target:?} names no pane in {app_id}'s layout"),
        ));
    };
    spec.query = query;
    spec.view_kind = ViewKindId::from(view_kind.to_string());
    if let Some(params) = params {
        let previous = std::mem::take(&mut spec.params);
        spec.params = params
            .into_iter()
            .map(
                |wanted| match previous.iter().find(|p| p.decl.name == wanted.decl.name) {
                    Some(kept) => ParamBinding {
                        decl: wanted.decl,
                        source: kept.source.clone(),
                    },
                    None => wanted,
                },
            )
            .collect();
    }
    let set = layout
        .set_pane(
            app_id.to_string(),
            device,
            LayoutPaneRefDto::tile(impress_layout::TileId::new(tile)),
            spec,
            actor,
            None,
        )
        .await;
    if !set.ok {
        return Err(layout_refused(set.code, set.message));
    }
    Ok((tile, set.focused == Some(tile), set.affected_panes))
}

/// The verbs an `{"open": …}` effect is: with a `target` role, that pane's
/// query and view kind are replaced (its role, channel, params and session
/// kept — what [`show_in_pane`] does with one `set_pane`); with none, the
/// focused pane is split vertically and the new pane, which takes focus,
/// shows the query. Pane references are resolved when the gesture applies,
/// against the tree as the verbs before them left it.
fn open_verbs(query: Value, view_kind: &str, target: Option<&str>) -> Result<Vec<Verb>> {
    let query: PaneQuery = serde_json::from_value(query)
        .map_err(|e| Refusal::invalid_argument(format!("open: query: {e}")))?;
    let view_kind = ViewKindId::from(view_kind.to_string());
    Ok(match target {
        Some(role) => {
            let target = LayoutPaneRefDto::role(role)
                .to_pane_ref()
                .map_err(Refusal::invalid_argument)?;
            vec![
                Verb::SetQuery {
                    target: target.clone(),
                    query,
                },
                Verb::SetViewKind { target, view_kind },
            ]
        }
        None => vec![Verb::Split {
            target: PaneRef::Focused,
            dir: LinearDir::Vertical,
            after: true,
            new: Some(PaneSpec::new(query, view_kind)),
        }],
    })
}

/// The verb a `{"publish": …}` effect is: a selection of `ids` under `kind`
/// on `pane`'s channel — what `layout-service_select` builds.
fn publish_verb(pane: &PaneHandle, kind: &str, ids: Value) -> Result<Verb> {
    let id_strings: Vec<String> = match ids {
        Value::Array(items) => items
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        Value::String(s) => vec![s],
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    };
    let ids = id_strings
        .iter()
        .map(|id| {
            id.trim()
                .parse::<ItemId>()
                .map_err(|e| Refusal::invalid_argument(format!("'{id}' is not an item id: {e}")))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Verb::Select {
        target: PaneRef::id(TileId::new(pane.tile)),
        kind: kind.trim().to_string(),
        ids,
    })
}

/// The pane parameters a surface's declared `params` become when it is
/// shown: each follows the window's default channel for its kind (a schema
/// ref is read as its pane-query kind, the way `publish` does).
pub(crate) fn pane_params_for(spec: &SurfaceSpec, manifest: &KindManifest) -> Vec<ParamBinding> {
    spec.params
        .iter()
        .map(|decl| ParamBinding {
            decl: ParamDecl {
                name: decl.name.clone(),
                kind: as_layout_kind(&decl.kind, manifest),
                required: decl.required,
            },
            source: ParamSource::Channel {
                channel: ChannelId::Follow,
            },
        })
        .collect()
}

/// `item(id)` of the `surface` kind — the query every surface pane's spec is
/// (ADR-0033 D1). Kept here so `surface_show` (service.rs) and this module's
/// own tests build it identically.
pub(crate) fn surface_item_query(id: ItemId) -> PaneQuery {
    PaneQuery {
        kinds: vec!["surface".to_string()],
        scope: Scope::Item {
            id: ItemRef::Id { id },
        },
        ..PaneQuery::default()
    }
}

// ---------------------------------------------------------------------------
// SurfaceRuntime
// ---------------------------------------------------------------------------

/// How long a source that failed is left alone before a render asks for it
/// again with the same arguments (RS-S15). A render happens on every
/// dispatch and every feed notification; without this, a verb whose app is
/// not running was called again — synchronously, over HTTP — each time, and
/// up to `sources + 1` times within one render.
pub const FAILED_SOURCE_BACKOFF: Duration = Duration::from_secs(5);

/// A source fetch that failed, and with which arguments.
#[derive(Debug, Clone)]
struct FailedFetch {
    args_hash: u64,
    at: Instant,
}

/// One `(surface, host)` instance, live in memory: its spec, its working
/// state, the pane parameters bound to it, and the source cache
/// [`impress_surface::plan`] reads. Also the sole owner of which pane (if
/// any) `surface_show` last put it in.
///
/// A runtime is a CACHE of the store, never a second truth (RS-S1): every
/// [`SessionRegistry::with`] compares the stored spec and state with what
/// this runtime last loaded or wrote, and reloads whichever moved — so an
/// update or state write from another handle, an HTTP request or another
/// process is what the next render shows, and the next dispatch builds on.
pub struct SurfaceRuntime {
    pub surface_id: ItemId,
    pub host: String,
    pub spec: SurfaceSpec,
    pub state: Value,
    /// The values of the surface's declared `params` for this call, by name
    /// (the `param` template root and a query source's bindings) — set by
    /// [`Self::bind_params`] before every render and dispatch, from the
    /// caller's explicit `params` or else from the pane that shows the
    /// surface (review RS-S3, AC-F15).
    pub params: Value,
    /// The spec's `revision`, as last loaded.
    pub revision: u64,
    /// The state row's revision, as last read or written — `None` while the
    /// instance runs on the spec's initial state.
    pub state_revision: Option<u64>,
    pub cache: SourceCache,
    /// Why a source's last fetch failed, by name — cleared when it succeeds.
    /// `render` hands it to `resolve_with_source_errors` so the pane says
    /// "source 'libs' failed: imbib is not running" rather than the generic
    /// "did not resolve". Before this the error was dropped on the floor.
    pub source_errors: BTreeMap<String, String>,
    pub pane: Option<PaneHandle>,
    /// The spec text this runtime was loaded from (see the struct docs).
    spec_text: String,
    /// The state text last read from or written to the store — `None` while
    /// the instance has no state row (it runs on the spec's initial state).
    state_text: Option<String>,
    /// See [`FAILED_SOURCE_BACKOFF`].
    failed: BTreeMap<String, FailedFetch>,
    /// The record kinds this runtime reads its `query` sources and published
    /// kinds in — the registry's, passed at [`SessionRegistry::with_manifest`].
    manifest: Arc<KindManifest>,
}

impl SurfaceRuntime {
    fn load(
        surface_id: ItemId,
        host: String,
        row: SurfaceRow,
        state: Option<(String, u64)>,
        manifest: Arc<KindManifest>,
    ) -> Self {
        let mut runtime = Self {
            surface_id,
            host,
            state: row.spec.state.clone(),
            spec: row.spec,
            params: Value::Object(serde_json::Map::new()),
            revision: row.revision,
            state_revision: None,
            cache: SourceCache::new(),
            source_errors: BTreeMap::new(),
            pane: None,
            spec_text: row.spec_text,
            state_text: None,
            failed: BTreeMap::new(),
            manifest,
        };
        runtime.adopt_state(state);
        runtime
    }

    /// Bring this runtime up to the store: a new spec replaces the old one
    /// (and every cached source, which may no longer mean what it did); a
    /// new state replaces the working state. Unchanged text is left alone,
    /// so a warm source cache survives every call that changed nothing.
    fn refresh(&mut self, row: SurfaceRow, state: Option<(String, u64)>) {
        self.revision = row.revision;
        if row.spec_text != self.spec_text {
            self.spec = row.spec;
            self.spec_text = row.spec_text;
            self.cache.clear();
            self.source_errors.clear();
            self.failed.clear();
            // No state row: the instance runs on the spec's initial state,
            // which is the NEW spec's now.
            if state.is_none() {
                self.state = self.spec.state.clone();
            }
        }
        self.state_revision = state.as_ref().map(|(_, revision)| *revision);
        let state_text = state.map(|(text, _)| text);
        if state_text != self.state_text {
            self.adopt_state_text(state_text);
        }
    }

    fn adopt_state(&mut self, state: Option<(String, u64)>) {
        self.state_revision = state.as_ref().map(|(_, revision)| *revision);
        self.adopt_state_text(state.map(|(text, _)| text));
    }

    fn adopt_state_text(&mut self, state_text: Option<String>) {
        self.state = match state_text.as_deref().map(serde_json::from_str::<Value>) {
            Some(Ok(state)) => state,
            // An unreadable state row is not a reason to refuse the surface:
            // it runs on the spec's initial state, and the next dispatch
            // writes a readable one over it.
            Some(Err(_)) | None => self.spec.state.clone(),
        };
        self.state_text = state_text;
    }

    /// Bind the surface's declared `params` for this call:
    ///
    /// 1. `explicit`, when the caller passed params — they are the whole
    ///    binding, and a name the spec does not declare is refused;
    /// 2. else, when a pane shows this instance (bound by the host, set by
    ///    `surface_show`, or found in the layout), that pane's bindings for
    ///    each declared name — the pane's own `params` as the layout
    ///    resolves them (a fixed id, or its channel's selection of the
    ///    param's kind);
    /// 3. else nothing: an unbound param leaves `{{param.x}}` a placeholder
    ///    and a query source's `required` `$x` refused.
    ///
    /// The pane's `item` binding is never a surface param: it names the
    /// surface itself.
    pub async fn bind_params(
        &mut self,
        executor: &dyn Executor,
        explicit: Option<&ParamsArg>,
    ) -> Result<()> {
        let declared: BTreeSet<String> = self.spec.params.iter().map(|p| p.name.clone()).collect();
        let bound: BTreeMap<String, String> = match explicit {
            Some(params) => {
                if let Some(extra) = params.keys().find(|k| !declared.contains(*k)) {
                    return Err(Refusal::invalid_argument(format!(
                        "params: this surface declares no param '{extra}' (it declares: {})",
                        if declared.is_empty() {
                            "none".to_string()
                        } else {
                            declared.iter().cloned().collect::<Vec<_>>().join(", ")
                        }
                    )));
                }
                params.clone()
            }
            None if declared.is_empty() => BTreeMap::new(),
            None => match self.pane_or_lookup(executor).await {
                Some(pane) => executor
                    .pane_bindings(&pane)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(name, _)| declared.contains(name) && name != "item")
                    .collect(),
                None => BTreeMap::new(),
            },
        };
        self.params = Value::Object(
            bound
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect(),
        );
        Ok(())
    }

    /// What this instance's last render or dispatch was built from.
    pub fn revisions(&self) -> Revisions {
        Revisions {
            revision: Some(self.revision),
            state_revision: self.state_revision,
            params: self
                .params
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Every source's value as the last render left it: `value` sources from
    /// the spec, fetched ones from the cache.
    fn source_values(&self) -> Value {
        let mut values = serde_json::Map::new();
        for (name, source) in &self.spec.sources {
            if let Source::Value { value } = source {
                values.insert(name.clone(), value.clone());
            }
        }
        for (name, cached) in self.cache.iter() {
            values.insert(name.clone(), cached.value.clone());
        }
        Value::Object(values)
    }

    /// The schema refs this runtime's `query` sources read, plus the
    /// literal (`Kind::Ref`) reads declared on the verb of every `verb`
    /// source (EF-4, RS-S2) — what a store invalidation has to name for
    /// [`Self::invalidate_sources`] to re-run one. A `verb` source whose verb
    /// is not linked, or whose reads are not literal refs (`target`/
    /// `children`/`prefix`/`any`, which need the resolved arguments the spy
    /// resolves at run time, not this static walk), contributes nothing —
    /// it still re-runs on an argument change or an explicit refresh.
    pub fn query_refs(&self) -> BTreeSet<String> {
        let manifest = &self.manifest;
        self.spec
            .sources
            .values()
            .filter_map(|source| match source {
                Source::Query { query } => Some(refs_read_by(query, manifest)),
                Source::Verb { verb, .. } => Some(verb_declared_read_refs(verb)),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Drop the cached value of every `query` source that reads one of
    /// `refs`, and every `verb` source whose verb declares one of `refs`
    /// among its reads (EF-4, RS-S2), so the next render runs it again
    /// (ADR-0033 Defaults: sources re-run "when a store invalidation names a
    /// query" — extended here to a verb's declared reads). Returns the names
    /// dropped. `value` sources are untouched (they have nothing to fetch),
    /// and a `verb` source whose reads are not literal refs is untouched
    /// too: it still re-runs when its arguments change or an action
    /// refreshes it.
    pub fn invalidate_sources(&mut self, refs: &BTreeSet<String>) -> Vec<String> {
        let manifest = &self.manifest;
        let names: Vec<String> = self
            .spec
            .sources
            .iter()
            .filter_map(|(name, source)| match source {
                Source::Query { query }
                    if refs_read_by(query, manifest)
                        .iter()
                        .any(|r| refs.contains(r)) =>
                {
                    Some(name.clone())
                }
                Source::Verb { verb, .. }
                    if verb_declared_read_refs(verb)
                        .iter()
                        .any(|r| refs.contains(r)) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        for name in &names {
            self.cache.remove(name);
            self.failed.remove(name);
        }
        names
    }

    /// Runtime inventory and liveness are memory state, not store mutations.
    /// Invalidate just sources that observe that inventory or call a provider.
    fn inventory_sources(&self) -> Vec<String> {
        self.spec
            .sources
            .iter()
            .filter_map(|(name, source)| {
                let Source::Verb { verb, .. } = source else {
                    return None;
                };
                let observes_inventory = matches!(
                    verb.as_str(),
                    "capabilities-service_list-verbs"
                        | "capabilities-service_verb-surface"
                        | "capabilities-service_catalogue-surface"
                        | "provider-service_list"
                );
                let calls_provider = impress_service_core::call::find(verb)
                    .is_some_and(|descriptor| descriptor.provider_id().is_some());
                (observes_inventory || calls_provider).then(|| name.clone())
            })
            .collect()
    }

    fn invalidate_inventory_sources(&mut self) {
        for name in self.inventory_sources() {
            self.cache.remove(&name);
            self.failed.remove(&name);
        }
    }

    /// Fetch every stale/unfetched source (bounded rounds: a chain of N
    /// dependent sources settles in at most N rounds, and a round that
    /// fetches nothing new stops immediately rather than spinning on a
    /// permanently failing verb) and resolve the tree.
    ///
    /// A source that failed with the same arguments within
    /// [`FAILED_SOURCE_BACKOFF`] is not asked again — in a later round of
    /// this render or in a later render — and keeps its error (RS-S15).
    pub async fn render(&mut self, executor: &dyn Executor) -> RenderTree {
        let max_rounds = self.spec.sources.len() + 1;
        for _ in 0..max_rounds {
            let requests = plan(&self.spec, &self.state, &self.params, &self.cache);
            if requests.is_empty() {
                break;
            }
            let mut progressed = false;
            for request in requests {
                if self.failed.get(&request.name).is_some_and(|f| {
                    f.args_hash == request.args_hash && f.at.elapsed() < FAILED_SOURCE_BACKOFF
                }) {
                    continue;
                }
                let fetched = match &request.kind {
                    SourceRequestKind::Verb { verb, args } => {
                        executor.call_verb(verb, args.clone()).await
                    }
                    SourceRequestKind::Query { query } => {
                        let decls = query_decls(&self.spec.params, &self.manifest);
                        let bindings = bindings_from_params(&decls, &self.params);
                        executor.run_query(query, &decls, &bindings).await
                    }
                };
                match fetched {
                    Ok(value) => {
                        self.source_errors.remove(&request.name);
                        self.failed.remove(&request.name);
                        self.cache.insert(
                            request.name.clone(),
                            CachedSource {
                                args_hash: request.args_hash,
                                value,
                            },
                        );
                        progressed = true;
                    }
                    Err(why) => {
                        tracing::warn!(
                            target: "surface",
                            "surface {} ({}): source '{}' failed [{}]: {}",
                            self.surface_id,
                            self.host,
                            request.name,
                            why.code,
                            why.message
                        );
                        self.source_errors.insert(request.name.clone(), why.message);
                        self.failed.insert(
                            request.name.clone(),
                            FailedFetch {
                                args_hash: request.args_hash,
                                at: Instant::now(),
                            },
                        );
                    }
                }
                // A failed fetch leaves the previous cache entry (if any) in
                // place — the last known value is still the best available
                // approximation (`plan.rs`'s own module docs make the same
                // call for a merely-stale entry).
            }
            if !progressed {
                break;
            }
        }
        // `{"value": …}` sources never go through `plan()`/fetch at all
        // (`impress_surface::plan`'s own module docs), so the source map
        // handed to `resolve` includes them directly.
        resolve_with_source_errors(
            &self.spec,
            &self.state,
            &self.params,
            &self.source_values(),
            Some(&self.source_errors),
        )
    }

    /// Every source that failed on the last render, in the wire's shape.
    pub fn source_error_list(&self) -> Vec<crate::dto::SourceError> {
        self.source_errors
            .iter()
            .map(|(name, message)| crate::dto::SourceError {
                name: name.clone(),
                message: message.clone(),
            })
            .collect()
    }

    /// Reduce an event, persist the resulting state, run every effect, and
    /// re-render. Persistence happens here (not in the caller) so a
    /// dispatch that runs zero effects still leaves the state row in sync
    /// with what `reduce` just decided.
    ///
    /// The state is written only when it differs from what is stored
    /// (RS-S14): once after `reduce`, before any effect runs — so an agent
    /// woken by an `emit` reads the state that produced it — and again only
    /// if a `call … into` changed it after that.
    ///
    /// # One click, one layout step (review PH-M2)
    ///
    /// Effects run in the order `reduce` gave them, except that a run of
    /// consecutive `publish`/`open` effects is ONE layout gesture: their
    /// verbs are gathered and applied together by
    /// [`Executor::apply_layout`] when the run ends, as one revision of the
    /// layout row and one undo entry, all or none. `[publish, open]` is one
    /// ⌘Z. A run ends at a `call` or an `emit`, which runs after the gesture
    /// before it has landed and before the one after it starts, so
    /// `[publish, call, open]` is two steps with the call between them —
    /// exactly the order it had when each effect was its own step. (No
    /// later effect reads a `call`'s result within one dispatch — `reduce`
    /// resolved them all up front — but a verb may read or change the
    /// layout, and an agent woken by an `emit` reads it.) A `refresh` ends
    /// nothing. The undo entry lands on the ring
    /// `impress_layout::UndoStacks::apply_all` picks: the first recorded
    /// verb's, so a gesture that starts with a `publish` lands on the
    /// surface pane's own ring.
    ///
    /// Every effect still reports its own outcome, in order. A gesture
    /// member refused before anything applied (no pane, a query that does
    /// not parse, an id that is not one) reports its refusal as before, and
    /// every other member of the gesture reports `not applied`, with that
    /// refusal's code. A gesture the layout refuses reports the layout's
    /// refusal on every member. Either way the layout is untouched.
    pub async fn dispatch(
        &mut self,
        executor: &dyn Executor,
        surfaces: &SurfaceStore,
        event: &Event,
        actor: ActorKind,
    ) -> Result<(RenderTree, Vec<EffectOutcomeDto>)> {
        // Actions read sources as the last render left them (RS-S8).
        let sources = self.source_values();
        let (new_state, effects) = reduce(&self.spec, &self.state, &self.params, &sources, event)
            .map_err(|e| Refusal::new(e.code(), e.to_string()))?;
        self.state = new_state;
        self.persist_state(surfaces, actor)?;

        let mut outcomes = Vec::with_capacity(effects.len());
        // Consecutive `publish`/`open` effects are one layout gesture (one
        // step, one undo entry, all or none); see this method's docs.
        let mut gesture: Option<Gesture> = None;
        for effect in effects {
            match effect {
                Effect::Publish { .. } | Effect::Open { .. } => {
                    let index = outcomes.len();
                    let kind = effect_kind(&effect);
                    match self.layout_effect(executor, effect).await {
                        Ok((pane, verbs, done)) => {
                            // A gesture is one layout's: an effect whose
                            // pane is in another (the pane moved apps
                            // mid-dispatch) starts its own.
                            if gesture
                                .as_ref()
                                .and_then(|g| g.pane.as_ref())
                                .is_some_and(|p| p.app_id != pane.app_id || p.device != pane.device)
                            {
                                flush_gesture(executor, gesture.take(), &mut outcomes, actor).await;
                            }
                            let pending = gesture.get_or_insert_with(Gesture::default);
                            pending.pane.get_or_insert(pane);
                            pending.verbs.extend(verbs);
                            pending.members.push(index);
                            outcomes.push(match &pending.refused {
                                Some((by, refusal)) => not_applied(kind, by, refusal),
                                None => EffectOutcomeDto::done(kind, done),
                            });
                        }
                        Err(refusal) => {
                            // Refused before anything applied, so the whole
                            // gesture is refused with it: what came before
                            // it in this click, and what comes after.
                            let pending = gesture.get_or_insert_with(Gesture::default);
                            if pending.refused.is_none() {
                                for &member in &pending.members {
                                    let was = outcomes[member].kind.clone();
                                    outcomes[member] = not_applied(&was, kind, &refusal);
                                }
                                pending.refused = Some((kind.to_string(), refusal.clone()));
                            }
                            pending.members.push(index);
                            outcomes.push(EffectOutcomeDto::failed(kind, refusal));
                        }
                    }
                }
                effect => {
                    // `call` and `emit` reach past this surface — a verb
                    // that may read or change the layout, an agent woken by
                    // the event — so each sees the layout the effects before
                    // it left: the gesture so far lands first, and the
                    // layout effects after it are a gesture of their own.
                    // `refresh` only drops a cached source; it ends nothing.
                    if !matches!(effect, Effect::Refresh { .. }) {
                        flush_gesture(executor, gesture.take(), &mut outcomes, actor).await;
                    }
                    let outcome = self.run_effect(executor, effect, actor).await;
                    outcomes.push(outcome);
                }
            }
        }
        flush_gesture(executor, gesture.take(), &mut outcomes, actor).await;
        for outcome in &outcomes {
            if outcome.ok {
                tracing::debug!(
                    target: "surface",
                    "surface {} ({}): {} effect ok: {}",
                    self.surface_id,
                    self.host,
                    outcome.kind,
                    outcome.message
                );
            } else {
                tracing::warn!(
                    target: "surface",
                    "surface {} ({}): {} effect failed [{}]: {}",
                    self.surface_id,
                    self.host,
                    outcome.kind,
                    outcome.code.as_deref().unwrap_or("?"),
                    outcome.message
                );
            }
        }
        self.persist_state(surfaces, actor)?;

        let tree = self.render(executor).await;
        Ok((tree, outcomes))
    }

    /// Write the working state if it differs from the stored one (or, with
    /// no state row yet, from the spec's initial state). Returns whether it
    /// wrote.
    fn persist_state(&mut self, surfaces: &SurfaceStore, actor: ActorKind) -> Result<bool> {
        let text = serde_json::to_string(&self.state)
            .map_err(|e| Refusal::internal(format!("encode surface state: {e}")))?;
        let unchanged = match &self.state_text {
            Some(stored) => *stored == text,
            None => serde_json::to_string(&self.spec.state).ok().as_deref() == Some(&text),
        };
        if unchanged {
            return Ok(false);
        }
        let revision = surfaces.set_state_text(self.surface_id, &self.host, text.clone(), actor)?;
        self.state_text = Some(text);
        self.state_revision = Some(revision);
        Ok(true)
    }

    /// Set the working state directly (`surface_state_set`), writing it
    /// through so the runtime and the store agree.
    pub fn set_state(
        &mut self,
        surfaces: &SurfaceStore,
        state: Value,
        actor: ActorKind,
    ) -> Result<()> {
        let text = serde_json::to_string(&state)
            .map_err(|e| Refusal::internal(format!("encode surface state: {e}")))?;
        let revision = surfaces.set_state_text(self.surface_id, &self.host, text.clone(), actor)?;
        self.state = state;
        self.state_text = Some(text);
        self.state_revision = Some(revision);
        Ok(())
    }

    /// The pane this instance is shown in — remembered from `surface_show`
    /// / `bind_pane` while the layout still shows it there (RS-S25), else
    /// recovered from the layout and remembered. See
    /// `Executor::pane_showing` for why the lookup exists.
    async fn pane_or_lookup(&mut self, executor: &dyn Executor) -> Option<PaneHandle> {
        if let Some(pane) = self.pane.clone() {
            if executor.pane_shows(&pane, self.surface_id).await {
                return Some(pane);
            }
            self.pane = None;
        }
        self.pane = executor.pane_showing(self.surface_id).await;
        self.pane.clone()
    }

    async fn run_effect(
        &mut self,
        executor: &dyn Executor,
        effect: Effect,
        actor: ActorKind,
    ) -> EffectOutcomeDto {
        match effect {
            Effect::Call {
                verb,
                mut args,
                into,
                adverbs,
            } => {
                // Adverbs ride beside the arguments under one reserved key; the
                // pipeline strips it (`Adverbs::take_from`).
                if let Some(adverbs) = adverbs {
                    if !args.is_object() {
                        args = Value::Object(Default::default());
                    }
                    if let Some(obj) = args.as_object_mut() {
                        obj.insert(impress_service_core::Adverbs::KEY.to_string(), adverbs);
                    }
                }
                let called = executor.call_verb(&verb, args).await;
                match called {
                    Ok(value) => {
                        if let Some(path) = &into {
                            if let Err(e) = state_path::write(&mut self.state, path, value) {
                                return EffectOutcomeDto::failed(
                                    "call",
                                    Refusal::new(e.code(), format!("{verb} ran, but `into`: {e}")),
                                );
                            }
                        }
                        EffectOutcomeDto::done("call", format!("called {verb}"))
                    }
                    Err(e) => EffectOutcomeDto::failed("call", e),
                }
            }
            Effect::Emit { name, payload } => {
                match executor
                    .emit(self.surface_id, &self.host, &name, payload, actor)
                    .await
                {
                    Ok(seq) => {
                        EffectOutcomeDto::done("emit", format!("emitted '{name}' (seq {seq})"))
                    }
                    Err(e) => EffectOutcomeDto::failed("emit", e),
                }
            }
            Effect::Refresh { source } => {
                self.cache.remove(&source);
                self.failed.remove(&source);
                EffectOutcomeDto::done(
                    "refresh",
                    format!("'{source}' will re-fetch on next render"),
                )
            }
            // Compiled by `layout_effect` and applied as a gesture by
            // `dispatch`; never run one by one.
            effect @ (Effect::Publish { .. } | Effect::Open { .. }) => {
                let kind = effect_kind(&effect);
                EffectOutcomeDto::failed(
                    kind,
                    Refusal::internal(format!("a '{kind}' effect is applied as a layout gesture")),
                )
            }
        }
    }

    /// A `publish` or `open` effect, compiled: the pane whose layout it
    /// changes, the layout verbs it is, and the message it reports when the
    /// gesture it joins lands. Refused, as before, when no pane shows this
    /// surface or the effect's own arguments do not make a verb.
    async fn layout_effect(
        &mut self,
        executor: &dyn Executor,
        effect: Effect,
    ) -> Result<(PaneHandle, Vec<Verb>, String)> {
        match effect {
            Effect::Publish { ids } => {
                let Some(pane) = self.pane_or_lookup(executor).await else {
                    return Err(Refusal::new(
                        "no-pane",
                        "no pane shows this surface yet (surface_show has not run)",
                    ));
                };
                let (kind, ids) = publish_kind_and_ids(&self.spec, ids, &self.manifest);
                let verb = publish_verb(&pane, &kind, ids)?;
                Ok((pane, vec![verb], format!("published on kind '{kind}'")))
            }
            Effect::Open {
                query,
                view_kind,
                target,
            } => {
                let Some(pane) = self.pane_or_lookup(executor).await else {
                    return Err(Refusal::new(
                        "no-pane",
                        "this surface instance has no pane yet (surface_show has not run) — \
                         nothing to open a query beside",
                    ));
                };
                let verbs = open_verbs(query, &view_kind, target.as_deref())?;
                Ok((pane, verbs, format!("opened a '{view_kind}' pane")))
            }
            other => Err(Refusal::internal(format!(
                "a '{}' effect is not a layout verb",
                effect_kind(&other)
            ))),
        }
    }
}

/// The layout effects of one dispatch that land together: one step in one
/// layout, all or none (review PH-M2). See [`SurfaceRuntime::dispatch`].
#[derive(Default)]
struct Gesture {
    /// The pane whose layout (app and device) the gesture changes — `None`
    /// only while every member so far was refused before it had one.
    pane: Option<PaneHandle>,
    verbs: Vec<Verb>,
    /// Which of the dispatch's outcomes are this gesture's, by index.
    members: Vec<usize>,
    /// The member refused before anything applied, and why: set, nothing
    /// in the gesture is applied.
    refused: Option<(String, Refusal)>,
}

/// Apply a gathered gesture, and when the layout refuses it, report that on
/// every member: none of them happened.
async fn flush_gesture(
    executor: &dyn Executor,
    gesture: Option<Gesture>,
    outcomes: &mut [EffectOutcomeDto],
    actor: ActorKind,
) {
    let Some(Gesture {
        pane: Some(pane),
        verbs,
        members,
        refused: None,
    }) = gesture
    else {
        return;
    };
    if verbs.is_empty() {
        return;
    }
    if let Err(refusal) = executor.apply_layout(&pane, verbs, actor).await {
        let together = members.len() > 1;
        for member in members {
            let kind = outcomes[member].kind.clone();
            let mut refusal = refusal.clone();
            if together {
                refusal.message = format!("{} (one gesture: none of it applied)", refusal.message);
            }
            outcomes[member] = EffectOutcomeDto::failed(&kind, refusal);
        }
    }
}

/// A member of a gesture another member's refusal kept from applying. It
/// carries that refusal's code, so a caller branching on codes sees why.
fn not_applied(kind: &str, refused_kind: &str, refusal: &Refusal) -> EffectOutcomeDto {
    EffectOutcomeDto::failed(
        kind,
        Refusal::new(
            refusal.code.clone(),
            format!(
                "not applied: the '{refused_kind}' in the same gesture was refused: {}",
                refusal.message
            ),
        ),
    )
}

/// An effect's `kind`, as its outcome names it.
fn effect_kind(effect: &Effect) -> &'static str {
    match effect {
        Effect::Call { .. } => "call",
        Effect::Publish { .. } => "publish",
        Effect::Emit { .. } => "emit",
        Effect::Open { .. } => "open",
        Effect::Refresh { .. } => "refresh",
    }
}

/// `human` | `agent` | `system` — the spelling layout verbs take as `actor`.
pub fn actor_name(actor: ActorKind) -> &'static str {
    match actor {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    }
}

/// Which record kind and which ids a `{"publish": {…}}` action resolves to.
///
/// `Action::Publish` carries no `kind` field (`impress-surface`'s
/// vocabulary — see `docs/plan-agent-surfaces.md`'s Actions table), so this
/// is a choice S4 had to make (see this crate's report): an explicit
/// `{"kind": ..., "ids": [...]}` object wins; otherwise the surface's FIRST
/// declared `params` entry names the kind a publish on this surface is FOR
/// (the same kind a downstream pane would bind `param.selected` to), falling
/// back to the generic `"item"` kind when the surface declares no params.
fn publish_kind_and_ids(
    spec: &SurfaceSpec,
    ids: Value,
    manifest: &KindManifest,
) -> (String, Value) {
    if let Value::Object(map) = &ids {
        if let (Some(Value::String(kind)), Some(inner)) = (map.get("kind"), map.get("ids")) {
            return (as_layout_kind(kind, manifest), inner.clone());
        }
    }
    let kind = spec
        .params
        .first()
        .map(|p| as_layout_kind(&p.kind, manifest))
        .unwrap_or_else(|| "item".to_string());
    (kind, ids)
}

/// A channel key in the LAYOUT's vocabulary.
///
/// A pane parameter declares a `RecordKindId` from
/// `impress_core::pane_query::KindManifest` — `publication` — while a surface
/// spec's own `params` may name the schema ref instead
/// (`imbib/bibliography-entry`, which is what the worked example and the
/// agent-surfaces doc use). Publishing under the ref put
/// `{"imbib/bibliography-entry": [...]}` on the channel, where a detail pane
/// bound to `publication` looked and found nothing: the row was selected in
/// the surface and the detail pane kept saying "No Selection" (verified live
/// in the impress window, 2026-09-22).
///
/// Both spellings are accepted rather than one being declared wrong, because
/// the vocabulary is documented with the ref and a spec already written that
/// way must keep working. An unknown string passes through untouched — a
/// surface may publish a kind this build has never heard of.
fn as_layout_kind(kind: &str, manifest: &KindManifest) -> String {
    if manifest.kinds.contains_key(kind) {
        return kind.to_string();
    }
    let base = kind.split_once('@').map(|(head, _)| head).unwrap_or(kind);
    for (id, refs) in &manifest.kinds {
        if refs
            .iter()
            .any(|r| r == kind || r.split_once('@').map(|(head, _)| head).unwrap_or(r) == base)
        {
            return id.clone();
        }
    }
    kind.to_string()
}

/// The schema refs a pane query reads: those of each kind it names, by the
/// manifest (a name the manifest does not know is taken to be a ref itself),
/// or every kind's when it names none — the same "no kinds means all kinds"
/// the compiler applies.
fn refs_read_by(query: &PaneQuery, manifest: &KindManifest) -> Vec<String> {
    if query.kinds.is_empty() {
        return manifest.kinds.values().flatten().cloned().collect();
    }
    query
        .kinds
        .iter()
        .flat_map(|kind| {
            manifest
                .kinds
                .get(kind)
                .cloned()
                .unwrap_or_else(|| vec![kind.clone()])
        })
        .collect()
}

/// The literal schema refs a linked verb declares among its reads (EF-4,
/// RS-S2) — `Kind::Ref` only. `Kind::Target`/`Children`/`Prefix`/`Any` name a
/// kind that depends on the resolved arguments (EF-2), which this static
/// walk does not have; those are left for the store spy (E2) to verify at
/// run time, and a `verb` source over one of them keeps re-running only on
/// an argument change or a refresh, exactly as before this change. A verb
/// name `find()` cannot resolve (not linked into this binary) reads nothing,
/// by the same "no kinds means all kinds" default this file avoids for
/// queries — a source over an unknown verb is not this function's problem to
/// solve.
fn verb_declared_read_refs(verb: &str) -> Vec<String> {
    match impress_service_core::call::find(verb) {
        Some(descriptor) => descriptor
            .effects()
            .reads
            .iter()
            .filter_map(|kind| match kind {
                impress_service_core::descriptor::Kind::Ref(r) => Some(r.to_string()),
                _ => None,
            })
            .collect(),
        None => Vec::new(),
    }
}

/// A surface's declared params as the query compiler reads them: a schema
/// ref (`imbib/bibliography-entry`, the spelling the vocabulary documents)
/// is its pane-query kind (`publication`), the same reading `publish` gives
/// it — else a param bound to a paper is "the wrong kind" for a query over
/// papers.
fn query_decls(params: &[ParamDecl], manifest: &KindManifest) -> Vec<ParamDecl> {
    params
        .iter()
        .map(|decl| ParamDecl {
            name: decl.name.clone(),
            kind: as_layout_kind(&decl.kind, manifest),
            required: decl.required,
        })
        .collect()
}

fn bindings_from_params(decls: &[ParamDecl], params: &Value) -> Bindings {
    let mut bindings = Bindings::new();
    if let Some(obj) = params.as_object() {
        for decl in decls {
            if let Some(Value::String(raw)) = obj.get(&decl.name) {
                if let Ok(id) = raw.parse::<ItemId>() {
                    bindings = bindings.with(decl.name.clone(), id);
                }
            }
        }
    }
    bindings
}

// ---------------------------------------------------------------------------
// SessionRegistry
// ---------------------------------------------------------------------------

/// One `(surface, host)` entry: the runtime behind an async lock, plus the
/// two sets the invalidation feed reads and writes WITHOUT that lock — so a
/// store write is recorded even while a render holds the runtime for the
/// length of a slow verb.
#[derive(Default)]
struct Slot {
    runtime: tokio::sync::Mutex<Option<SurfaceRuntime>>,
    /// The refs the runtime's query sources read (see
    /// [`SurfaceRuntime::query_refs`]), refreshed on every call.
    reads: Mutex<BTreeSet<String>>,
    /// Refs a store invalidation named since the runtime last ran; applied
    /// with [`SurfaceRuntime::invalidate_sources`] on the next call.
    dirty: Mutex<BTreeSet<String>>,
    /// Set by [`SessionRegistry::retry_failed_sources`]: forget every
    /// remembered failure on the next call.
    retry_failed: std::sync::atomic::AtomicBool,
    reads_inventory: std::sync::atomic::AtomicBool,
    inventory_dirty: std::sync::atomic::AtomicBool,
}

type SessionMap = HashMap<(ItemId, String), Arc<Slot>>;

/// The `(surface, host)` runtime table — the same shape
/// `impress-layout-service::SessionRegistry` gives the layout tree.
///
/// # One per store per process (RS-S1, SK-K1, AC-F1)
///
/// Every caller that renders or drives a surface on one store in one process
/// shares one registry: `impress-store-ffi`'s `SharedStore` owns it and
/// hands it to every `SharedSurface` (each pane, the HTTP bridge), and the
/// store the process installed as its own (`impress_store_service`) uses
/// [`SessionRegistry::shared`], which is what `DefaultImpressSurfaceService::new`
/// reads. Each registry also re-checks the store on every call (see
/// [`Self::with`]), which is what keeps two PROCESSES — the app and
/// `impress-mcp` — from serving each other stale specs.
///
/// # Concurrency
///
/// A call holds its entry's async lock for its whole length, so two calls on
/// the same `(surface, host)` run one after the other and neither's result
/// is overwritten by the other's stale copy; calls on different entries run
/// in parallel.
pub struct SessionRegistry {
    sessions: Mutex<SessionMap>,
    /// The record kinds every runtime in this registry reads in — handed to
    /// each [`SurfaceRuntime`] as it loads.
    manifest: Arc<KindManifest>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    /// A registry over the [`process_manifest::current`] — what a process that never
    /// chose a manifest runs (the process-wide [`Self::shared`] one, and the
    /// tests). A host with its own record kinds uses [`Self::with_manifest`].
    pub fn new() -> Self {
        Self::with_manifest(process_manifest::current())
    }

    /// A registry whose runtimes read `manifest`'s record kinds.
    pub fn with_manifest(manifest: impl Into<Arc<KindManifest>>) -> Self {
        Self {
            sessions: Mutex::new(SessionMap::new()),
            manifest: manifest.into(),
        }
    }

    pub fn shared() -> Arc<SessionRegistry> {
        static GLOBAL: OnceLock<Arc<SessionRegistry>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Arc::new(SessionRegistry::new()))
            .clone()
    }

    /// Run `f` against the runtime for `(surface, host)`, after bringing it
    /// up to the store: loaded on first touch, and on every later touch the
    /// stored spec and state are read (two keyed reads) and whichever moved
    /// since this runtime last loaded or wrote it is reloaded. A surface
    /// that no longer exists is an error and its entry is dropped.
    pub async fn with<R>(
        &self,
        surfaces: &SurfaceStore,
        surface_id: ItemId,
        host: &str,
        f: impl for<'a> FnOnce(
            &'a mut SurfaceRuntime,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<R>> + Send + 'a>,
        >,
    ) -> Result<R> {
        let key = (surface_id, host.to_string());
        let slot = self.lock().entry(key.clone()).or_default().clone();
        let mut guard = slot.runtime.lock().await;

        let Some(row) = surfaces.get(surface_id)? else {
            *guard = None;
            drop(guard);
            self.forget(surface_id, host);
            return Err(Refusal::not_found(format!("no surface {surface_id}")));
        };
        let state = surfaces.get_state_entry(surface_id, host)?;
        let runtime = match guard.as_mut() {
            Some(runtime) => {
                runtime.refresh(row, state);
                runtime
            }
            None => guard.insert(SurfaceRuntime::load(
                surface_id,
                host.to_string(),
                row,
                state,
                self.manifest.clone(),
            )),
        };
        *lock_set(&slot.reads) = runtime.query_refs();
        slot.reads_inventory.store(
            !runtime.inventory_sources().is_empty(),
            std::sync::atomic::Ordering::SeqCst,
        );
        if slot
            .inventory_dirty
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            runtime.invalidate_inventory_sources();
        }
        let dirty = std::mem::take(&mut *lock_set(&slot.dirty));
        if !dirty.is_empty() {
            runtime.invalidate_sources(&dirty);
        }
        if slot
            .retry_failed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            runtime.failed.clear();
        }
        f(runtime).await
    }

    /// A store write touched records of these schema refs: mark every
    /// runtime whose query sources read one of them, so its next call
    /// re-runs those sources (RS-S2, AC-F16). Returns the surfaces marked —
    /// the ones a host should re-render. Never waits on a runtime in use.
    pub fn invalidate_refs(&self, refs: &BTreeSet<String>) -> Vec<ItemId> {
        if refs.is_empty() {
            return Vec::new();
        }
        let mut marked = Vec::new();
        for ((surface_id, _host), slot) in self.lock().iter() {
            let reads = lock_set(&slot.reads);
            let hit: BTreeSet<String> = reads.intersection(refs).cloned().collect();
            drop(reads);
            if hit.is_empty() {
                continue;
            }
            lock_set(&slot.dirty).extend(hit);
            if !marked.contains(surface_id) {
                marked.push(*surface_id);
            }
        }
        marked
    }

    /// What can answer a verb changed (a verb host was installed): let every
    /// runtime ask its failed sources again on its next call instead of
    /// waiting out [`FAILED_SOURCE_BACKOFF`].
    pub fn retry_failed_sources(&self) {
        for slot in self.lock().values() {
            slot.retry_failed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A host observed a provider registry revision change. Mark affected
    /// cached sources and return the surfaces the native feed must re-render.
    /// No store write and no network call happen here.
    pub fn invalidate_provider_inventory(&self) -> Vec<ItemId> {
        let mut surfaces = BTreeSet::new();
        for ((surface, _), slot) in self.lock().iter() {
            if slot
                .reads_inventory
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                slot.inventory_dirty
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                surfaces.insert(*surface);
            }
        }
        surfaces.into_iter().collect()
    }

    /// Every schema ref some live runtime's query sources read — what a feed
    /// watching other processes' writes needs to look for.
    pub fn watched_refs(&self) -> BTreeSet<String> {
        self.lock()
            .values()
            .flat_map(|slot| lock_set(&slot.reads).clone())
            .collect()
    }

    pub fn forget(&self, surface_id: ItemId, host: &str) {
        self.lock().remove(&(surface_id, host.to_string()));
    }

    /// Drop every `(surface_id, *)` runtime, on every host. Called after
    /// `surface_delete`. (An update needs no forget: the next call sees the
    /// new spec in the store and reloads it.)
    pub fn forget_surface(&self, surface_id: ItemId) {
        self.lock().retain(|(id, _), _| *id != surface_id);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, SessionMap> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn lock_set(set: &Mutex<BTreeSet<String>>) -> MutexGuard<'_, BTreeSet<String>> {
    set.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod flatten_tests {
    use super::flatten_payloads;
    use serde_json::json;

    /// The shape a `table`'s `columns` actually name — the S1 golden's rows
    /// are flat, and the live window drew two empty rows until they were.
    #[test]
    fn a_rows_payload_fields_read_at_the_top_of_the_row() {
        let rows = json!([{
            "id": "row-1",
            "created": "2026-09-22T00:00:00Z",
            "payload": {"title": "A dark matter survey", "year": 2024},
        }]);
        let flat = flatten_payloads(rows);
        let row = &flat[0];
        assert_eq!(row["title"], json!("A dark matter survey"));
        assert_eq!(row["year"], json!(2024));
        // The envelope survives: `id` is what a selection publishes.
        assert_eq!(row["id"], json!("row-1"));
        assert_eq!(row["payload"]["title"], json!("A dark matter survey"));
    }

    /// A domain field may not shadow an envelope field. `id` is the one that
    /// matters — publishing a payload's `id` would select the wrong item.
    #[test]
    fn the_envelope_wins_a_name_collision() {
        let rows = json!([{"id": "envelope", "payload": {"id": "domain", "title": "t"}}]);
        let flat = flatten_payloads(rows);
        assert_eq!(flat[0]["id"], json!("envelope"));
        assert_eq!(flat[0]["title"], json!("t"));
    }

    /// Anything that is not an array of objects with an object payload is
    /// returned untouched: a verb source's value passes through this path too.
    #[test]
    fn a_row_without_a_payload_object_is_unchanged() {
        for value in [json!([{"id": "x"}]), json!({"not": "an array"}), json!([7])] {
            assert_eq!(flatten_payloads(value.clone()), value);
        }
    }
}

#[cfg(test)]
mod publish_kind_tests {
    use super::as_layout_kind as kind_in;
    use impress_pane_query_compile::test_support::builtin_manifest;

    fn as_layout_kind(kind: &str) -> String {
        kind_in(kind, &builtin_manifest())
    }

    /// The channel key a pane parameter can actually bind to.
    #[test]
    fn a_schema_ref_becomes_the_manifest_kind_id() {
        assert_eq!(as_layout_kind("imbib/bibliography-entry"), "publication");
        assert_eq!(as_layout_kind("task@1.0.0"), "task");
    }

    #[test]
    fn a_kind_id_passes_through() {
        assert_eq!(as_layout_kind("publication"), "publication");
        assert_eq!(as_layout_kind("collection"), "collection");
    }

    /// A surface may publish a kind this build has never heard of; that is
    /// the surface's business, not something to rewrite or refuse.
    #[test]
    fn an_unknown_kind_is_left_alone() {
        assert_eq!(as_layout_kind("nobody/owns-this"), "nobody/owns-this");
    }
}

#[cfg(test)]
mod call_verb_tests {
    use super::{call_verb, provider_result, Executor, SurfaceRuntime};
    use impress_item::item::ActorKind;
    use impress_layout::Verb;
    use impress_pane_query::Bindings;
    use impress_service_core::descriptor::{Safety, SafetyClass};
    use impress_service_core::descriptor_handle::VerbHandle;
    use impress_service_core::provider::{ProviderStatus, ProviderVerb};
    use impress_service_core::refusal::codes;
    use impress_service_core::{McpToolDescriptor, ServiceFuture};
    use impress_surface::{Event, PaneQuery, ParamDecl, SurfaceSpec};
    use koine_store_memory::MemoryStore;
    use serde_json::{json, Value};
    use std::sync::Arc;

    use crate::store::SurfaceStore;

    const FAILING: &str = "call-verb-test_always-fails";

    fn always_fails(_: Value) -> ServiceFuture {
        Box::pin(async { Err("boom".into()) })
    }

    // A handler that errors: every real verb answers a refusal as an `ok:
    // false` envelope, so none of them reaches the `Handler` arm.
    fn object_schema() -> Value {
        json!({"type": "object"})
    }

    fn provider_handle() -> VerbHandle {
        let safety = Safety {
            class: SafetyClass::ReadOnly,
            idempotent: true,
        };
        VerbHandle::Provider(Arc::new(ProviderVerb {
            name: "fixture-provider-service_echo".into(),
            service: "fixture-provider-service".into(),
            method: "echo".into(),
            description: "fixture provider response".into(),
            input_schema: object_schema(),
            output_schema: object_schema(),
            declared_safety: safety,
            effective_safety: safety,
            since: "0.1.0".into(),
            examples: vec![],
            provider_id: "fixture-provider".into(),
            status: ProviderStatus::Unavailable,
            deprecated_since: None,
            generation: 1,
        }))
    }

    struct ProviderReplyExecutor(Value);

    #[async_trait::async_trait]
    impl Executor for ProviderReplyExecutor {
        async fn call_verb(&self, name: &str, _args: Value) -> super::Result<Value> {
            assert_eq!(name, "fixture-provider-service_echo");
            provider_result(provider_handle(), self.0.clone())
        }

        async fn run_query(
            &self,
            _query: &PaneQuery,
            _decls: &[ParamDecl],
            _bindings: &Bindings,
        ) -> super::Result<Value> {
            unreachable!("fixture has no query")
        }

        async fn apply_layout(
            &self,
            _pane: &super::PaneHandle,
            _verbs: Vec<Verb>,
            _actor: ActorKind,
        ) -> super::Result<()> {
            unreachable!("fixture has no layout effect")
        }

        async fn emit(
            &self,
            _surface: impress_item::item::ItemId,
            _host: &str,
            _name: &str,
            _payload: Value,
            _actor: ActorKind,
        ) -> super::Result<u64> {
            unreachable!("fixture has no emit effect")
        }
    }

    fn provider_surface() -> SurfaceSpec {
        serde_json::from_value(json!({
            "surface": "1.0",
            "name": "Provider refusal fixture",
            "state": { "answer": null },
            "sources": {
                "remote": { "verb": "fixture-provider-service_echo", "args": {} }
            },
            "root": { "column": [
                { "text": "{{source.remote.value}}", "id": "remote-value" },
                { "button": { "label": "Run", "on_click": [
                    { "call": {
                        "verb": "fixture-provider-service_echo",
                        "args": {},
                        "into": "state.answer"
                    } }
                ] }, "id": "run" }
            ] }
        }))
        .unwrap()
    }

    async fn surface_with_reply(
        reply: Value,
    ) -> (SurfaceRuntime, Vec<crate::dto::EffectOutcomeDto>) {
        let store = Arc::new(MemoryStore::new());
        let surfaces = SurfaceStore::new(store);
        let row = surfaces
            .create(&provider_surface(), None, &[], ActorKind::Agent)
            .unwrap();
        let mut runtime = SurfaceRuntime::load(
            row.id,
            "fixture-host".into(),
            row,
            None,
            Arc::new(impress_pane_query_compile::test_support::builtin_manifest()),
        );
        let executor = ProviderReplyExecutor(reply);
        runtime.render(&executor).await;
        let event: Event = serde_json::from_value(json!({
            "widget": "run", "kind": "click", "value": null
        }))
        .unwrap();
        let (_, effects) = runtime
            .dispatch(&executor, &surfaces, &event, ActorKind::Agent)
            .await
            .unwrap();
        (runtime, effects)
    }

    static FAILING_VERB: impress_service_core::VerbDescriptor =
        impress_service_core::VerbDescriptor {
            name: FAILING,
            service: "call-verb-test",
            method: "always-fails",
            description: "test only: a handler that always errors",
            input_schema: object_schema,
            output_schema: object_schema,
            safety: impress_service_core::Safety {
                class: impress_service_core::SafetyClass::ReadOnly,
                idempotent: true,
            },
            since: "0.1.0",
            effects: impress_service_core::Effects::NONE,
            deprecated: None,
            aliases: &[],
            supports: &[],
            adjective: None,
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            returns_kind: None,
            source: impress_service_core::Source::Linked,
            handler: always_fails,
        };
    impress_service_core::inventory::submit! { McpToolDescriptor::of(&FAILING_VERB) }

    /// The texts and codes `impress_service_core::call` hands back are the
    /// ones this crate's own copy used to write (review RS-S21): a source or
    /// an effect reports them unchanged.
    #[tokio::test]
    async fn an_unknown_verb_and_a_failed_one_keep_their_codes_and_text() {
        let unknown = call_verb("no-such-tool", json!({})).await.unwrap_err();
        assert_eq!(unknown.code, "unknown-verb");
        assert_eq!(unknown.message, "Unknown tool: no-such-tool");

        let failed = call_verb(FAILING, json!({})).await.unwrap_err();
        assert_eq!(failed.code, codes::VERB_FAILED);
        assert_eq!(failed.message, format!("{FAILING}: boom"));
    }

    #[tokio::test]
    async fn provider_refusals_fail_sources_and_effects_without_caching_data() {
        for code in [codes::HOST_UNAVAILABLE, "review-pending"] {
            let (runtime, effects) = surface_with_reply(json!({
                "ok": false,
                "code": code,
                "message": format!("fixture-provider-service_echo refused: {code}"),
                "wire_version": 1
            }))
            .await;
            assert!(!runtime.cache.contains_key("remote"), "{code}");
            assert!(runtime.source_errors["remote"].contains(code), "{code}");
            assert_eq!(effects.len(), 1);
            assert!(!effects[0].ok);
            assert_eq!(effects[0].code.as_deref(), Some(code));
            assert!(runtime.state["answer"].is_null());
        }
    }

    #[tokio::test]
    async fn successful_provider_data_and_linked_results_stay_unchanged() {
        let data = json!({"value": 42});
        let (runtime, effects) = surface_with_reply(data.clone()).await;
        assert!(runtime.source_errors.is_empty());
        assert_eq!(runtime.cache["remote"].value, data);
        assert!(effects[0].ok);
        assert_eq!(runtime.state["answer"], data);

        let linked_result = json!({
            "ok": false,
            "code": "domain-status",
            "message": "linked result owns its contract",
            "wire_version": 1
        });
        assert_eq!(
            provider_result(VerbHandle::Linked(&FAILING_VERB), linked_result.clone()).unwrap(),
            linked_result
        );
    }

    /// An executor that records the arguments `call_verb` was handed.
    struct RecordingExecutor(std::sync::Mutex<Option<Value>>);

    #[async_trait::async_trait]
    impl Executor for RecordingExecutor {
        async fn call_verb(&self, _name: &str, args: Value) -> super::Result<Value> {
            *self.0.lock().unwrap() = Some(args);
            Ok(json!({}))
        }

        async fn run_query(
            &self,
            _query: &PaneQuery,
            _decls: &[ParamDecl],
            _bindings: &Bindings,
        ) -> super::Result<Value> {
            unreachable!("fixture has no query")
        }

        async fn apply_layout(
            &self,
            _pane: &super::PaneHandle,
            _verbs: Vec<Verb>,
            _actor: ActorKind,
        ) -> super::Result<()> {
            unreachable!("fixture has no layout effect")
        }

        async fn emit(
            &self,
            _surface: impress_item::item::ItemId,
            _host: &str,
            _name: &str,
            _payload: Value,
            _actor: ActorKind,
        ) -> super::Result<u64> {
            unreachable!("fixture has no emit effect")
        }
    }

    #[tokio::test]
    async fn a_call_effect_forwards_its_adverbs_under_the_reserved_key() {
        let store = Arc::new(MemoryStore::new());
        let surfaces = SurfaceStore::new(store);
        let row = surfaces
            .create(&provider_surface(), None, &[], ActorKind::Agent)
            .unwrap();
        let mut runtime = SurfaceRuntime::load(
            row.id,
            "fixture-host".into(),
            row,
            None,
            Arc::new(impress_pane_query_compile::test_support::builtin_manifest()),
        );
        let executor = RecordingExecutor(Default::default());

        let with = runtime
            .run_effect(
                &executor,
                impress_surface::reduce::Effect::Call {
                    verb: "any-service_verb".into(),
                    args: json!({"id": "x"}),
                    into: None,
                    adverbs: Some(json!({"tentatively": true})),
                },
                ActorKind::Agent,
            )
            .await;
        assert!(with.ok);
        let seen = executor.0.lock().unwrap().take().unwrap();
        assert_eq!(seen["adverbs"], json!({"tentatively": true}));
        assert_eq!(seen["id"], json!("x"));

        runtime
            .run_effect(
                &executor,
                impress_surface::reduce::Effect::Call {
                    verb: "any-service_verb".into(),
                    args: json!({"id": "x"}),
                    into: None,
                    adverbs: None,
                },
                ActorKind::Agent,
            )
            .await;
        let seen = executor.0.lock().unwrap().take().unwrap();
        assert!(seen.get("adverbs").is_none());
    }
}
