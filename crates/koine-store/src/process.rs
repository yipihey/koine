//! The process-wide store registry, over the port.
//!
//! A service built with no explicit store (the `#[impress_service]`
//! constructor the inventory calls with no arguments) needs *the* store of
//! this process. The host puts it here once; every runtime crate asks
//! [`current`] and never names a backend:
//!
//! * [`install`] / [`install_as_fallback`] set the store. A fallback is a
//!   stand-in the host substituted for a store it could not open (an empty
//!   in-memory one): a verb that writes asks [`is_fallback`] and refuses
//!   rather than answering `ok` into a store that vanishes with the process.
//! * [`set_opener`] registers a lazy opener. [`acquire`] calls it when
//!   nothing real is installed, so a host that opens its store on first use
//!   (with a deadline, falling back when the open is slow) keeps doing so; the
//!   opener installs what it got. A fallback is never final: the next
//!   [`acquire`] calls the opener again.
//! * A verb running under `pipeline::invoke_on(store, …)` carries that store
//!   for the call. It wins over the installed one, provided the store's
//!   concrete type is known to [`accept_override_of`] (the memory and
//!   directory backends are; a host registers its own).
//! * [`set_subtree_resolver`] / [`subtree_resolver`]: the same registry for
//!   how a collection's descendants are found. The default is `RootOnly`.
//! * [`install_blobs`] / [`blobs`]: the blob view of the store (plan-koine-live
//!   L-D6). Installing it also fills `impress_service_core`'s blob slot, so a
//!   verb that takes a `BlobRef` reads its bytes through [`BlobStore`].

use std::any::{Any, TypeId};
use std::sync::{Arc, RwLock};

use impress_item::{AttributedStore, BlobStore};
use impress_pane_query_compile::{RootOnly, SubtreeResolver};

/// A subtree resolver a process shares between threads.
pub type SharedSubtreeResolver = Arc<dyn SubtreeResolver + Send + Sync>;

struct Slot {
    store: Option<Arc<dyn AttributedStore>>,
    fallback: bool,
    intended: Option<String>,
    opener: Option<fn()>,
    resolver: Option<SharedSubtreeResolver>,
    wideners: Vec<(TypeId, Widener)>,
}

type Widener = fn(Arc<dyn Any + Send + Sync>) -> Option<Arc<dyn AttributedStore>>;

static SLOT: RwLock<Slot> = RwLock::new(Slot {
    store: None,
    fallback: false,
    intended: None,
    opener: None,
    resolver: None,
    wideners: Vec::new(),
});

fn read() -> std::sync::RwLockReadGuard<'static, Slot> {
    SLOT.read().unwrap_or_else(|e| e.into_inner())
}

fn write() -> std::sync::RwLockWriteGuard<'static, Slot> {
    SLOT.write().unwrap_or_else(|e| e.into_inner())
}

/// Install the process's store. A later install replaces the earlier one.
pub fn install(store: Arc<dyn AttributedStore>) {
    let mut slot = write();
    slot.store = Some(store);
    slot.fallback = false;
    slot.intended = None;
}

/// Install a stand-in for a store that could not be opened. It answers
/// [`current`] like any store and [`is_fallback`] says `true`, until a real
/// [`install`] replaces it.
pub fn install_as_fallback(store: Arc<dyn AttributedStore>) {
    let mut slot = write();
    slot.store = Some(store);
    slot.fallback = true;
    slot.intended = None;
}

/// [`install_as_fallback`], saying where the real store was meant to be (a
/// path or URL), so a refusal can name it: [`intended_location`].
pub fn install_as_fallback_for(store: Arc<dyn AttributedStore>, intended: impl Into<String>) {
    let mut slot = write();
    slot.store = Some(store);
    slot.fallback = true;
    slot.intended = Some(intended.into());
}

/// Where the real store was meant to be, when the installed one is a
/// stand-in installed with [`install_as_fallback_for`].
pub fn intended_location() -> Option<String> {
    let slot = read();
    if slot.fallback {
        slot.intended.clone()
    } else {
        None
    }
}

/// Whether a store is installed (real or fallback).
pub fn is_installed() -> bool {
    read().store.is_some()
}

/// Whether the installed store is a stand-in ([`install_as_fallback`]).
pub fn is_fallback() -> bool {
    let slot = read();
    slot.store.is_some() && slot.fallback
}

/// Register the function [`acquire`] calls when no real store is installed.
/// The function installs what it opened ([`install`], or
/// [`install_as_fallback`] when it could not open the real one).
pub fn set_opener(opener: fn()) {
    write().opener = Some(opener);
}

/// Let a call-scoped store of concrete type `T` (the value a caller put in
/// `Call::store` for `pipeline::invoke_on`) answer [`acquire`] for the verbs
/// of that call. `MemoryStore` and `DirStore` are accepted already.
pub fn accept_override_of<T: AttributedStore + 'static>() {
    let mut slot = write();
    register_widener::<T>(&mut slot.wideners);
}

fn register_widener<T: AttributedStore + 'static>(wideners: &mut Vec<(TypeId, Widener)>) {
    let id = TypeId::of::<T>();
    if wideners.iter().any(|(known, _)| *known == id) {
        return;
    }
    wideners.push((id, |erased| {
        erased
            .downcast::<T>()
            .ok()
            .map(|store| store as Arc<dyn AttributedStore>)
    }));
}

fn scoped_store() -> Option<Arc<dyn AttributedStore>> {
    let erased = impress_service_core::pipeline::context::store_override_erased()?;
    // Copy the table out so no lock is held while a widener runs.
    let mut wideners: Vec<Widener> = read().wideners.iter().map(|(_, w)| *w).collect();
    let mut builtin = Vec::new();
    register_widener::<koine_store_memory::MemoryStore>(&mut builtin);
    register_widener::<koine_store_dir::DirStore>(&mut builtin);
    wideners.extend(builtin.into_iter().map(|(_, w)| w));
    wideners.into_iter().find_map(|w| w(erased.clone()))
}

/// The store this call should use and whether it is a stand-in: the
/// call-scoped store when the call has one (never a stand-in), else the
/// installed one, opened lazily through [`set_opener`] when none is real yet.
/// `None` when the host installed nothing and registered no opener.
pub fn acquire() -> Option<(Arc<dyn AttributedStore>, bool)> {
    if let Some(store) = scoped_store() {
        return Some((store, false));
    }
    let opener = {
        let slot = read();
        match (&slot.store, slot.fallback, slot.opener) {
            (Some(store), false, _) => return Some((store.clone(), false)),
            (_, _, opener) => opener,
        }
    };
    // Not under the lock: the opener installs, and may take a while.
    if let Some(opener) = opener {
        opener();
    }
    let slot = read();
    slot.store.clone().map(|store| (store, slot.fallback))
}

/// [`acquire`], and when the host installed nothing at all, a private
/// in-memory stand-in installed as a fallback (once), so a service built with
/// no store still answers, flagged `true`, instead of panicking. A write to a
/// stand-in vanishes with the process; the flag is how a verb that writes
/// knows to refuse.
pub fn acquire_or_stand_in() -> (Arc<dyn AttributedStore>, bool) {
    if let Some(found) = acquire() {
        return found;
    }
    let mut slot = write();
    if slot.store.is_none() {
        slot.store = Some(Arc::new(koine_store_memory::MemoryStore::new()));
        slot.fallback = true;
        slot.intended = None;
    }
    let store = slot.store.clone().expect("just installed");
    (store, slot.fallback)
}

/// The store of this call or process; see [`acquire`].
pub fn current() -> Option<Arc<dyn AttributedStore>> {
    acquire().map(|(store, _)| store)
}

static BLOBS: RwLock<Option<Arc<dyn BlobStore>>> = RwLock::new(None);

/// Adapts the port's [`BlobStore`] to the reader `impress-service-core` (which
/// sits below the store) asks for.
struct BlobAdapter(Arc<dyn BlobStore>);

impl impress_service_core::BlobReader for BlobAdapter {
    fn read(&self, item: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
        let id = item
            .parse::<impress_item::ItemId>()
            .map_err(|e| format!("blob item `{item}` is not a uuid: {e}"))?;
        self.0.get_blob(id, name).map_err(|e| e.to_string())
    }
}

/// Install the process's blob store, and with it the reader a verb's
/// `BlobRef::read()` goes through. A later install replaces the earlier.
pub fn install_blobs(store: Arc<dyn BlobStore>) {
    impress_service_core::blob_ref::install_blob_reader(Arc::new(BlobAdapter(store.clone())));
    *BLOBS.write().unwrap_or_else(|e| e.into_inner()) = Some(store);
}

/// The installed blob store, if the host installed one.
pub fn blobs() -> Option<Arc<dyn BlobStore>> {
    BLOBS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Install how a collection's descendants are found (a store with a
/// collection tree resolves it; the default is the collection itself).
pub fn set_subtree_resolver(resolver: SharedSubtreeResolver) {
    write().resolver = Some(resolver);
}

/// The installed subtree resolver, or [`RootOnly`].
pub fn subtree_resolver() -> SharedSubtreeResolver {
    read()
        .resolver
        .clone()
        .unwrap_or_else(|| Arc::new(RootOnly))
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_store_memory::MemoryStore;
    use std::sync::Mutex;

    // The registry is process-wide; these cases take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn reset() {
        let mut slot = write();
        slot.store = None;
        slot.fallback = false;
        slot.intended = None;
        slot.opener = None;
        slot.resolver = None;
    }

    #[test]
    fn nothing_installed_answers_none() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(!is_installed());
        assert!(current().is_none());
        assert!(!is_fallback());
    }

    #[test]
    fn an_installed_store_is_the_current_one_and_not_a_fallback() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let store: Arc<dyn AttributedStore> = Arc::new(MemoryStore::new());
        install(store.clone());
        assert!(is_installed());
        assert!(!is_fallback());
        assert!(Arc::ptr_eq(&current().unwrap(), &store));
        reset();
    }

    #[test]
    fn a_fallback_is_flagged_and_a_real_install_replaces_it() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        install_as_fallback(Arc::new(MemoryStore::new()));
        assert!(is_fallback());
        assert!(acquire().unwrap().1);
        install(Arc::new(MemoryStore::new()));
        assert!(!is_fallback());
        reset();
    }

    #[test]
    fn a_host_that_installed_nothing_gets_a_flagged_stand_in_once() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let (first, fallback) = acquire_or_stand_in();
        assert!(fallback);
        let (second, _) = acquire_or_stand_in();
        assert!(
            Arc::ptr_eq(&first, &second),
            "one stand-in, not one per call"
        );
        assert!(is_fallback());
        install_as_fallback_for(Arc::new(MemoryStore::new()), "/nowhere/impress.sqlite");
        assert_eq!(
            intended_location().as_deref(),
            Some("/nowhere/impress.sqlite")
        );
        install(Arc::new(MemoryStore::new()));
        assert_eq!(intended_location(), None);
        reset();
    }

    #[test]
    fn the_opener_runs_when_nothing_real_is_installed_and_a_fallback_is_retried() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        set_opener(|| {
            let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                install_as_fallback(Arc::new(MemoryStore::new()));
            } else {
                install(Arc::new(MemoryStore::new()));
            }
        });
        assert!(acquire().unwrap().1, "first open fell back");
        assert!(!acquire().unwrap().1, "the retry opened the real store");
        let calls = CALLS.load(std::sync::atomic::Ordering::SeqCst);
        assert!(!acquire().unwrap().1);
        assert_eq!(
            CALLS.load(std::sync::atomic::Ordering::SeqCst),
            calls,
            "a real store is not reopened"
        );
        reset();
    }

    #[test]
    fn a_call_scoped_store_wins_over_the_installed_one() {
        use impress_service_core::pipeline::context::{
            sync_scope, CallContext, InverseSlot, MutationIds,
        };
        use impress_service_core::pipeline::CallerIdentity;
        use impress_service_core::Adverbs;
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let installed: Arc<dyn AttributedStore> = Arc::new(MemoryStore::new());
        install(installed.clone());
        let scoped = Arc::new(MemoryStore::new());
        let context = CallContext {
            call_id: "c".into(),
            trace_id: "t".into(),
            parent_call: None,
            caller: CallerIdentity::Person,
            verb: "x-service_y".into(),
            store_override: Some(scoped.clone() as Arc<dyn Any + Send + Sync>),
            mutation_ids: MutationIds::default(),
            adverbs: Adverbs::default(),
            inverse: InverseSlot::default(),
        };
        let seen = sync_scope(Arc::new(context), acquire).unwrap();
        assert!(!seen.1);
        assert!(!Arc::ptr_eq(&seen.0, &installed));
        assert!(Arc::ptr_eq(&acquire().unwrap().0, &installed));
        reset();
    }

    #[test]
    fn installed_blobs_answer_a_blob_ref_read() {
        use impress_item::ItemStore;
        use impress_service_core::{BlobError, BlobRef};
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let store = Arc::new(MemoryStore::new());
        let owner: impress_item::Item = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "schema": "paper", "payload": {},
            "created": "2026-01-01T00:00:00Z", "modified": "2026-01-01T00:00:00Z",
            "author": "t", "author_kind": "Human", "logical_clock": 0,
            "origin": null, "canonical_id": null, "tags": [], "flag": null,
            "is_read": false, "is_starred": false, "priority": "normal",
            "visibility": "private", "message_type": null, "produced_by": null,
            "version": null, "batch_id": null, "references": [], "parent": null
        }))
        .unwrap();
        let item = store.insert(owner).unwrap();
        store.put_blob(item, "a.csv", b"x,y").unwrap();
        install_blobs(store.clone());
        assert!(blobs().is_some());
        let blob = |item: String, name: &str| BlobRef {
            item,
            name: name.into(),
        };
        assert_eq!(blob(item.to_string(), "a.csv").read().unwrap(), b"x,y");
        assert!(matches!(
            blob(item.to_string(), "b.csv").read(),
            Err(BlobError::NotFound { .. })
        ));
        assert!(matches!(
            blob("not-a-uuid".into(), "a.csv").read(),
            Err(BlobError::Store(_))
        ));
        impress_service_core::blob_ref::clear_blob_reader();
        *BLOBS.write().unwrap() = None;
    }

    #[test]
    fn the_subtree_resolver_defaults_to_the_root() {
        let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let root = uuid::Uuid::new_v4();
        assert_eq!(subtree_resolver().subtree(root), vec![root]);
        set_subtree_resolver(Arc::new(|id| vec![id, id]));
        assert_eq!(subtree_resolver().subtree(root).len(), 2);
        reset();
    }
}
