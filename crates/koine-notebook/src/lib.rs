//! `koine-notebook`: the call log as a lab notebook (docs/plan-koine-notebook.md).
//!
//! A koine host calls [`install`] once its store is open: the
//! [`sink::StoreSink`] becomes the audit layer's sink, so every call record
//! the pipeline builds (with its step tree, read set, run, host and build)
//! is stored as a `core/verb-call@1.0.0` item, and a run left open by the
//! previous launch is open again. The `notebook-service` verbs
//! ([`service::NotebookService`]) read that log: runs, a timeline, one entry
//! with its steps and self times, notes, comparisons between runs, replay and
//! a Markdown export. [`NOTEBOOK_SURFACE`] is the surface over them that the
//! kit package ships (plan-koine-research-os B-D4).

pub mod analysis;
pub mod dto;
pub mod history;
pub mod records;
pub mod runs;
pub mod service;
pub mod sink;

use std::sync::{Arc, RwLock};
use std::time::Duration;

use impress_item::AttributedStore;
use impress_service_core::pipeline::audit;
use serde_json::{json, Value};

pub use service::{DefaultNotebookService, NotebookService};
pub use sink::{SinkConfig, StoreSink};

/// The Notebook surface: runs, the timeline, and the selected entry's steps,
/// self-time plot and notes, with Replay, Diagnose and Export.
pub const NOTEBOOK_SURFACE: &str = include_str!("../surfaces/notebook.surface.json");

/// The surface's name, as its spec says.
pub const NOTEBOOK_SURFACE_NAME: &str = "Notebook";

struct Installed {
    sink: Arc<StoreSink>,
    config: SinkConfig,
}

static INSTALLED: RwLock<Option<Installed>> = RwLock::new(None);

/// Start the store sink over `store`, install it as the audit sink, and
/// re-open a run the store says is open. Call once per host start; a later
/// call replaces the earlier sink.
pub fn install(store: Arc<dyn AttributedStore>, config: SinkConfig) -> Arc<StoreSink> {
    let sink = StoreSink::start(store.clone(), config.clone());
    audit::install(sink.clone());
    if let Ok(mut slot) = INSTALLED.write() {
        *slot = Some(Installed {
            sink: sink.clone(),
            config,
        });
    }
    match runs::reopen(store.as_ref()) {
        Ok(Some(run)) => tracing::info!(target: "notebook", "run '{}' is open again", run.name),
        Ok(None) => {}
        Err(error) => tracing::warn!(target: "notebook", "open run not read: {error}"),
    }
    sink
}

/// The call log's writer as numbers: rows written, dropped for a full
/// channel and refused by the store; calls that found no sink at all
/// (the pipeline's count, which includes the drops); the channel's size.
pub fn health() -> serde_json::Value {
    use std::sync::atomic::Ordering;
    let (written, dropped, failed) = installed_sink().map_or((0, 0, 0), |sink| {
        let stats = sink.stats();
        (
            stats.written.load(Ordering::Relaxed),
            stats.dropped.load(Ordering::Relaxed),
            stats.failed.load(Ordering::Relaxed),
        )
    });
    serde_json::json!({
        "written": written,
        "dropped": dropped,
        "failed": failed,
        "no_sink": impress_service_core::pipeline::audit::dropped(),
        "channel_capacity": sink::CHANNEL_CAP,
    })
}

/// [`install`] for a host that opens its store lazily: the writer asks
/// `open` on its first record (`None` drops and counts it). An open run is
/// not re-opened, since that would open the store now.
pub fn install_with(open: sink::StoreOpener, config: SinkConfig) -> Arc<StoreSink> {
    let sink = StoreSink::start_lazy(open, config.clone());
    audit::install(sink.clone());
    if let Ok(mut slot) = INSTALLED.write() {
        *slot = Some(Installed {
            sink: sink.clone(),
            config,
        });
    }
    sink
}

/// The installed sink, if a host installed one.
pub fn installed_sink() -> Option<Arc<StoreSink>> {
    INSTALLED
        .read()
        .ok()
        .and_then(|slot| slot.as_ref().map(|i| i.sink.clone()))
}

/// Wait (briefly) for the records handed to the sink so far to be written,
/// so a verb that reads the log sees the calls that just finished.
pub(crate) fn flush() {
    if let Some(sink) = installed_sink() {
        sink.flush(Duration::from_secs(2));
    }
}

/// `(host, build)` to stamp a run with.
pub(crate) fn stamp() -> (String, Value) {
    INSTALLED
        .read()
        .ok()
        .and_then(|slot| {
            slot.as_ref().map(|i| {
                (
                    i.config.host.clone(),
                    json!({"app": i.config.app, "version": i.config.version}),
                )
            })
        })
        .unwrap_or_else(|| (String::new(), Value::Null))
}

/// Reference the service so the linker keeps its registrations.
pub fn ensure_linked() {
    std::hint::black_box(DefaultNotebookService::new as *const ());
    std::hint::black_box(history::DefaultHistoryService::new as *const ());
}
