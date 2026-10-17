//! The store sink: a koine host keeps its call log (plan-koine-notebook
//! N-D1, N-D6, N-D9).
//!
//! The audit layer hands every record to the installed `audit::Sink` and
//! must not block. [`StoreSink`] puts the record on a bounded channel and
//! returns; one writer thread turns each into a `core/verb-call@1.0.0` item
//! of the store it was started with. The writer runs outside any call, so
//! its own writes are never audited and never noted as a call's mutations.
//!
//! What is not written:
//!
//! - a record of a call that ran on a per-call store override (a Tier A
//!   scenario's store): counted in [`SinkStats::skipped`];
//! - a successful read-only record of the notebook's own plumbing (the
//!   surface wait loop and the notebook's read verbs, [`is_quiet`]), which
//!   would otherwise fill the log while a run records reads: counted in
//!   [`SinkStats::quiet`];
//! - a record the channel had no room for: counted in
//!   `audit::count_dropped()`, as the audit layer's contract asks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use chrono::Utc;
use impress_item::AttributedStore;
use impress_service_core::pipeline::audit::{self, VerbCallRecord};
use impress_service_core::{SafetyClass, VerbDescriptor};
use serde_json::{json, Value};

use crate::records;

/// Records waiting for the writer before a new one is dropped.
pub const CHANNEL_CAP: usize = 1024;
/// Records outside any run kept by the sink's own pruning (N-D9).
pub const DEFAULT_KEEP: usize = 5000;
/// The sink prunes after this many writes.
pub const PRUNE_EVERY: u64 = 512;

/// Who and what the records are stamped with (N-D6).
#[derive(Debug, Clone)]
pub struct SinkConfig {
    /// The host's device name.
    pub host: String,
    /// The app id.
    pub app: String,
    /// The app's version.
    pub version: String,
    /// Records outside any run to keep ([`DEFAULT_KEEP`]); `0` never prunes.
    pub keep: usize,
}

impl SinkConfig {
    pub fn new(
        host: impl Into<String>,
        app: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            app: app.into(),
            version: version.into(),
            keep: DEFAULT_KEEP,
        }
    }
}

/// What the sink did, for a status line and the tests.
#[derive(Debug, Default)]
pub struct SinkStats {
    pub written: AtomicU64,
    pub skipped: AtomicU64,
    pub quiet: AtomicU64,
    pub failed: AtomicU64,
    pub pruned: AtomicU64,
}

/// The audit sink a koine host installs.
pub struct StoreSink {
    sender: SyncSender<Message>,
    stats: Arc<SinkStats>,
}

enum Message {
    Record(Box<VerbCallRecord>),
    Flush(SyncSender<()>),
}

impl StoreSink {
    /// Start the writer thread over `store`. Install the result with
    /// `audit::install`.
    pub fn start(store: Arc<dyn AttributedStore>, config: SinkConfig) -> Arc<Self> {
        let (sender, receiver) = sync_channel(CHANNEL_CAP);
        let stats = Arc::new(SinkStats::default());
        let writer_stats = stats.clone();
        std::thread::Builder::new()
            .name("koine-call-log".into())
            .spawn(move || write_loop(receiver, store, config, writer_stats))
            .expect("spawn the call-log writer");
        Arc::new(Self { sender, stats })
    }

    pub fn stats(&self) -> &SinkStats {
        &self.stats
    }

    /// Wait until every record handed in before this call is written (at
    /// most `timeout`). A notebook verb calls it before reading, so a call
    /// that just finished is in the timeline.
    pub fn flush(&self, timeout: std::time::Duration) -> bool {
        let (done, wait) = sync_channel(1);
        if self.sender.send(Message::Flush(done)).is_err() {
            return false;
        }
        wait.recv_timeout(timeout).is_ok()
    }
}

impl audit::Sink for StoreSink {
    fn flush(&self, timeout: std::time::Duration) -> Result<(), String> {
        if StoreSink::flush(self, timeout) {
            Ok(())
        } else {
            Err("timed out waiting for the call-log writer".into())
        }
    }

    fn record(&self, record: VerbCallRecord) {
        if record.store_override.is_some() {
            self.stats.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if is_quiet(&record) {
            self.stats.quiet.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match self.sender.try_send(Message::Record(Box::new(record))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                audit::count_dropped();
            }
        }
    }
}

/// The verbs whose successful read-only calls are plumbing, not research:
/// the surface wait loop and render, and the notebook's own reads.
pub fn is_quiet(record: &VerbCallRecord) -> bool {
    if !record.ok {
        return false;
    }
    let name = record.verb.as_ref();
    let plumbing = name.ends_with("_surface-wait")
        || name.ends_with("_surface-render")
        || name.ends_with("_surface-events")
        || name.starts_with("notebook-service_");
    plumbing
        && VerbDescriptor::find(name).is_some_and(|verb| verb.safety.class == SafetyClass::ReadOnly)
}

fn write_loop(
    receiver: Receiver<Message>,
    store: Arc<dyn AttributedStore>,
    config: SinkConfig,
    stats: Arc<SinkStats>,
) {
    let mut since_prune = 0u64;
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Flush(done) => {
                let _ = done.send(());
            }
            Message::Record(record) => {
                let item = item_of(&record, &config);
                match store.insert(item) {
                    Ok(_) => {
                        stats.written.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error) => {
                        stats.failed.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(target: "notebook", call_id = %record.call_id,
                            "call record not written: {error}");
                    }
                }
                since_prune += 1;
                if config.keep > 0 && since_prune >= PRUNE_EVERY {
                    since_prune = 0;
                    match prune(store.as_ref(), config.keep) {
                        Ok(n) => {
                            stats.pruned.fetch_add(n, Ordering::Relaxed);
                        }
                        Err(error) => {
                            tracing::warn!(target: "notebook", "call log not pruned: {error}")
                        }
                    }
                }
            }
        }
    }
}

/// The item a record is stored as: the pipeline's payload plus `host` and
/// `build` (N-D6).
pub fn item_of(record: &VerbCallRecord, config: &SinkConfig) -> impress_item::Item {
    let mut payload = record.payload();
    payload.insert("host".into(), json!(config.host));
    payload.insert(
        "build".into(),
        json!({ "app": config.app, "version": config.version }),
    );
    payload.insert("caller_kind".into(), json!(record.caller.kind()));
    let created = chrono::DateTime::parse_from_rfc3339(&record.started_at)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    records::new_item(
        records::record_id(&record.call_id),
        records::VERB_CALL,
        payload,
        record.caller.author(),
        records::actor_kind(&record.caller),
        created,
        None,
    )
}

/// Delete the oldest call records outside any run beyond `keep`, with their
/// notes. Answers how many records were deleted.
pub fn prune(store: &dyn AttributedStore, keep: usize) -> Result<u64, String> {
    let all = records::all_of(store, records::VERB_CALL, &[], true)?;
    let outside: Vec<_> = all
        .iter()
        .filter(|item| {
            !matches!(item.payload.get("run"), Some(impress_item::Value::String(s)) if !s.is_empty())
        })
        .collect();
    if outside.len() <= keep {
        return Ok(0);
    }
    let notes = records::all_of(store, records::NOTE, &[], false)?;
    let mut deleted = 0;
    for item in &outside[keep..] {
        for note in notes.iter().filter(|n| n.parent == Some(item.id)) {
            store.delete(note.id).map_err(|e| e.to_string())?;
        }
        store.delete(item.id).map_err(|e| e.to_string())?;
        deleted += 1;
    }
    Ok(deleted)
}

/// The record a payload describes, for tests and readers: the payload's
/// `run` field, if it names one.
pub fn run_of(payload: &serde_json::Map<String, Value>) -> Option<&str> {
    payload
        .get("run")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}
