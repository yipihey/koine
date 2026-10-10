//! ADR-0033 surfaces: spec, schema, plan/resolve/reduce, pure.
//!
//! A **surface** is a small, closed, declarative UI document (a [`spec::SurfaceSpec`])
//! that an agent can author reliably and a validator can reject precisely — the same
//! discipline the pane-query algebra applies to queries (ADR-0031 D2), for the same
//! reason: no operators, no conditionals, no loops, nothing that needs a sandbox.
//! Anything that *computes* is a verb, called through the `#[impress_service]`
//! inventory in the host process (ADR-0033 D4) — this crate never calls one.
//!
//! This crate is the whole of ADR-0033 D2's "pure Rust function pair": it owns the
//! vocabulary ([`spec`]), the template language that fills it in ([`template`]), and
//! three pure functions built on top —
//!
//! - [`validate::validate`] — is a spec well-formed?
//! - [`plan::plan`] — which sources need fetching, in dependency order, skipping
//!   what a cache already has fresh?
//! - [`resolve::resolve`] — given fetched source data, what does the human see
//!   (a [`resolve::RenderTree`])?
//! - [`reduce::reduce`] — given something the human did (a [`spec::Event`]), what is
//!   the new state and what side effects (verb calls, publishes, opens, …) follow?
//!
//! **No I/O, no store, no async, no time.** Every function here is a value → value
//! mapping. The thing that runs verbs, holds the source cache across turns, and
//! talks to the store is `impress-surface-service` (S4), a sibling crate; the thing
//! that turns a [`resolve::RenderTree`] into pixels is a renderer (Swift today,
//! plain text in `koine-tui`) that holds no logic of its own (D2) — it is a mapping over
//! what this crate already computed. The public name of this layer is **koine**
//! (`crates/koine`); this crate remains the grammar.
//!
//! # Where the vocabulary is silent
//!
//! `docs/plan-agent-surfaces.md` ("The vocabulary (normative)") is authoritative and
//! is implemented exactly; every place it left a shape or a behaviour unspecified,
//! this crate picks the smallest thing and says so in a doc comment at the point of
//! the choice. The two biggest are noted here so they are not missed:
//!
//! - **Node kinds are closed, but a node's raw shape is not rejected wholesale on an
//!   unrecognized kind key.** [`spec::Node`] hand-rolls `Serialize`/`Deserialize`
//!   (rather than `#[derive]` + `#[serde(flatten)]` alone) precisely so that a kind
//!   this build does not know about survives parsing as [`spec::NodeKind::Unknown`]
//!   and degrades to a placeholder at *resolve* time (ADR-0033 "Defaults": "unknown
//!   widget kinds degrade to a placeholder that keeps the node, … so a spec authored
//!   for a newer kit still renders its rest"). A derive-only `#[serde(flatten)]` enum
//!   cannot do this: an unrecognized tag is a hard deserialize error, which would
//!   make a whole surface unparseable because of one node it doesn't need to run.
//! - **Handlers never appear in the [`resolve::RenderTree`].** The renderer stays
//!   dumb (D2): it forwards every interaction as a raw [`spec::Event`] naming the
//!   widget id, and [`reduce::reduce`] decides whether that widget has a matching
//!   handler (a no-op if not). The render tree therefore carries display data only,
//!   never `on_click`/`on_select`/`on_change` — the renderer does not need to know
//!   they exist to fire them.

pub mod plan;
pub mod reduce;
pub mod resolve;
pub mod spec;
pub mod state_path;
pub mod template;
pub mod validate;

mod example;

pub use example::{example_job_monitor, example_paper_triage, example_signal_explorer};
pub use plan::{plan, CachedSource, SourceCache, SourceRequest, SourceRequestKind};
pub use reduce::{reduce, Effect, ReduceError};
pub use resolve::{resolve, resolve_with_source_errors, RenderKind, RenderNode, RenderTree};
pub use spec::{
    child_pointers, handler_pointers, walk_actions, walk_with_pointers, Action, Button, Event,
    EventKind, FieldKind, Grid, Image, ListWidget, Node, NodeKind, PaneQuery, ParamDecl, Plot,
    Section, Source, Status, SurfaceSpec, Tab, Table, When, NODE_KIND_NAMES, SURFACE_VERSION,
};
pub use state_path::StatePathError;
pub use template::{Context, Template, TemplateError};
pub use validate::{validate, validate_json, Problem, Severity};
