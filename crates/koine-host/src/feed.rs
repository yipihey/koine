//! The store feed (plan-koine-live L-D2): a write wakes the surfaces that
//! read it.
//!
//! One thread, `koine-host-feed`, subscribes to the host's store with an empty
//! query and turns each [`ItemEvent`] into the schema refs it touched:
//!
//! | Event | Refs |
//! |---|---|
//! | `Created(item)` | the item's schema (learned) |
//! | `Updated { id }` | the schema the feed learned for `id`, else a `get` |
//! | `OperationApplied { target_id }` | the target's schema, the same way (a `dir:` or SQLite store reports every change as one) |
//! | `Deleted(id)` | the schema learned for `id`, forgotten; for an id never seen, every ref a live surface reads (the conservative answer: the row is gone, so its kind cannot be asked) |
//!
//! Events that are already waiting are drained together, so a burst is one
//! [`SessionRegistry::invalidate_refs`] call. That call marks the surfaces
//! whose query sources read one of the refs and advances their wake counter,
//! which is what `surface_wait` is polling; a ref that is the layout row's
//! schema also drops the layout service's cached session for this host's
//! `(app, device)`, so the next read re-reads the row another writer changed.
//!
//! A `dir:` store's watcher delivers another process's writes into the same
//! subscription, so two apps on one directory redraw each other; the memory
//! and SQLite stores deliver this process's writes, which is what a
//! background job's sample needs. Nothing is debounced: an event costs one
//! map lookup.
//!
//! Another process's write to a SQLite store reaches no subscription here
//! (SQLite events are per connection). An embedding app that hears of such a
//! write by some other means (impress's Darwin notification, say) calls
//! [`Waker::store_changed_elsewhere`], which wakes every surface: the note
//! does not say what changed. A note heard within [`OWN_ECHO`] of this
//! host's own write is taken for that write's echo and ignored.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use impress_item::schema::refs;
use impress_item::{AttributedStore, ItemEvent, ItemId, ItemQuery};
use impress_layout_service::DefaultLayoutService;
use impress_surface_service::SessionRegistry;

/// How long the thread waits for an event before it looks at the stop flag.
const POLL: Duration = Duration::from_millis(250);

/// How long after this host's own write a "changed elsewhere" call is taken
/// for that write's echo. impress posts its Darwin note within milliseconds
/// of a write and a listener polls at a quarter of a second, so an echo
/// lands well inside a second. The cost: another process's write in that
/// second wakes nothing until the next write is heard.
pub const OWN_ECHO: Duration = Duration::from_secs(1);

/// When the feed last heard this host's own write, as milliseconds since
/// `origin` plus one (zero: never).
#[derive(Clone)]
struct LastOwn {
    origin: Instant,
    at: Arc<AtomicU64>,
}

impl LastOwn {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            at: Arc::new(AtomicU64::new(0)),
        }
    }

    fn mark(&self) {
        let now = self.origin.elapsed().as_millis() as u64 + 1;
        self.at.store(now, Ordering::SeqCst);
    }

    fn within(&self, window: Duration) -> bool {
        let at = self.at.load(Ordering::SeqCst);
        at != 0 && self.origin.elapsed().as_millis() as u64 + 1 - at < window.as_millis() as u64
    }
}

/// Wakes every surface when the store changed in a way the feed cannot hear.
/// Cheap to clone; safe from any thread.
#[derive(Clone)]
pub struct Waker {
    registry: Arc<SessionRegistry>,
    layout: DefaultLayoutService,
    app_id: String,
    device: String,
    last_own: LastOwn,
}

impl Waker {
    /// Another process wrote the store: wake every surface that reads it,
    /// unless this host wrote within [`OWN_ECHO`] (then it is that write's
    /// echo, and the feed has already woken what it touched). `true` when
    /// it woke them.
    pub fn store_changed_elsewhere(&self) -> bool {
        if self.last_own.within(OWN_ECHO) {
            return false;
        }
        let mut refs = self.registry.watched_refs();
        refs.insert(refs::IMPRESS_UI_LAYOUT.as_str().to_string());
        self.layout.forget_session(&self.app_id, Some(&self.device));
        self.registry.invalidate_refs(&refs);
        true
    }
}

/// A running feed. Dropping it stops the thread at its next wake-up (within
/// a quarter of a second); [`Self::stop`] waits for that.
pub struct FeedHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    waker: Waker,
}

impl FeedHandle {
    /// A [`Waker`] for writes this feed cannot hear.
    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// Stop the thread and wait for it.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Subscribe to `store` and keep `registry` (and, for a layout write,
/// `layout`'s session for `app_id` on `device`) told what changed. A store
/// that cannot subscribe gets no feed: the handle is returned and the
/// thread ends at once, logging why.
pub fn start(
    store: Arc<dyn AttributedStore>,
    registry: Arc<SessionRegistry>,
    layout: DefaultLayoutService,
    app_id: String,
    device: String,
) -> FeedHandle {
    let stop = Arc::new(AtomicBool::new(false));
    // Subscribe here, not on the thread, so a write made right after `start`
    // returns is heard.
    let receiver = match store.subscribe(ItemQuery::default()) {
        Ok(receiver) => Some(receiver),
        Err(error) => {
            tracing::warn!(target: "koine-host", "store feed: cannot subscribe ({error}); \
                surfaces will not wake on a write");
            None
        }
    };
    let waker = Waker {
        registry,
        layout,
        app_id,
        device,
        last_own: LastOwn::new(),
    };
    let thread = receiver.and_then(|receiver| {
        let flag = stop.clone();
        let feed = waker.clone();
        std::thread::Builder::new()
            .name("koine-host-feed".into())
            .spawn(move || run(store, feed, receiver, flag))
            .map_err(|error| tracing::warn!(target: "koine-host", "store feed: {error}"))
            .ok()
    });
    FeedHandle {
        stop,
        thread,
        waker,
    }
}

fn run(
    store: Arc<dyn AttributedStore>,
    feed: Waker,
    receiver: Receiver<ItemEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut schemas: HashMap<ItemId, String> = HashMap::new();
    let layout_ref = refs::IMPRESS_UI_LAYOUT.as_str().to_string();
    while !stop.load(Ordering::SeqCst) {
        let first = match receiver.recv_timeout(POLL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        feed.last_own.mark();
        let mut touched = BTreeSet::new();
        let mut everything = false;
        let mut event = Some(first);
        while let Some(current) = event.take() {
            everything |= refs_of(&*store, &mut schemas, current, &mut touched);
            event = receiver.try_recv().ok();
        }
        if everything {
            touched.extend(feed.registry.watched_refs());
            touched.insert(layout_ref.clone());
        }
        if touched.is_empty() {
            continue;
        }
        if touched.contains(&layout_ref) {
            feed.layout.forget_session(&feed.app_id, Some(&feed.device));
        }
        feed.registry.invalidate_refs(&touched);
    }
}

/// Add the refs `event` touched to `out`, learning ids as it goes. `true`
/// when the event cannot be named (a delete of an id the feed never saw).
fn refs_of(
    store: &dyn AttributedStore,
    schemas: &mut HashMap<ItemId, String>,
    event: ItemEvent,
    out: &mut BTreeSet<String>,
) -> bool {
    match event {
        ItemEvent::Created(item) => {
            let schema = item.schema.as_str().to_string();
            schemas.insert(item.id, schema.clone());
            out.insert(schema);
        }
        ItemEvent::Updated { id, .. } | ItemEvent::OperationApplied { target_id: id, .. } => {
            if let Some(schema) = schema_of(store, schemas, id) {
                out.insert(schema);
            }
        }
        ItemEvent::Deleted(id) => match schemas.remove(&id) {
            Some(schema) => {
                out.insert(schema);
            }
            None => return true,
        },
    }
    false
}

fn schema_of(
    store: &dyn AttributedStore,
    schemas: &mut HashMap<ItemId, String>,
    id: ItemId,
) -> Option<String> {
    if let Some(schema) = schemas.get(&id) {
        return Some(schema.clone());
    }
    let schema = store.get(id).ok().flatten()?.schema.as_str().to_string();
    schemas.insert(id, schema.clone());
    Some(schema)
}
