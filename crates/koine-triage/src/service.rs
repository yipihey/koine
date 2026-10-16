//! `TriageService` — star, flag, tag and status over any item (ADR-0022 D5).
//!
//! The GUI triage menu is available on publications, manuscripts, figures,
//! messages, tasks and runs alike; before this, an agent had none of it. These
//! are thin wrappers over [`crate::ops`], which is itself a thin
//! wrapper over the store's `FieldMutation`s — no schema knowledge in either
//! layer, which is what lets one tool serve every record kind.
//!
//! ## Dismissal is not universal
//!
//! `set_status` writes the payload `status` string, the chassis-wide
//! status-change convention (`dismissed`, `archived` — see
//! `docs/status-lifecycle.md`). **Publications do not work that way.** imbib
//! dismisses a paper by moving it to the Dismissed *library*, guarded by the
//! "dismissed papers must never re-enter the inbox" invariant; writing
//! `status: "dismissed"` on a publication sets a field nothing reads and
//! dismisses nothing. Use imbib-core's own dismissal ops for papers.

use std::sync::Arc;

use impress_item::{AttributedStore, StoreError};
use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::ops as triage_ops;

/// What a triage mutation did.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TriageResult {
    pub ok: bool,
    /// The item that was triaged, echoed back.
    pub id: String,
    pub message: String,
}

/// Star / flag / tag / status on any item in the shared store.
///
/// Every method takes an item id (a lowercase UUID string) of ANY record kind:
/// publication, manuscript, figure, message, task, agent run. The store's
/// envelope carries star, flag and tags for all of them.
#[impress_service]
pub trait TriageService: Send + Sync + 'static {
    /// Star or unstar an item.
    #[impress_method]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000", "starred": true}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn set_starred(&self, id: String, starred: bool) -> TriageResult;

    /// Set an item's flag colour ("red", "orange", "blue", … — free-form), or
    /// clear the flag by passing null.
    #[impress_method]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000", "color": "red"}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn set_flag(&self, id: String, color: Option<String>) -> TriageResult;

    /// Add a tag to an item. Tag paths are hierarchical and slash-separated
    /// ("reading/queue"). Idempotent.
    #[impress_method]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000", "tag": "reading/queue"}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn add_tag(&self, id: String, tag: String) -> TriageResult;

    /// Remove a tag from an item. A tag the item does not carry is a no-op.
    #[impress_method]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000", "tag": "reading/queue"}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn remove_tag(&self, id: String, tag: String) -> TriageResult;

    /// Set the item's lifecycle `status`, or clear it with null.
    ///
    /// Two values are reserved suite-wide: `dismissed` (swept out of the
    /// working set — hidden everywhere but the Dismissed section, never
    /// destructive) and `archived` (a deliberate end-state for finished work).
    /// Everything else is schema-owned free text, e.g. a manuscript's
    /// `draft` | `internal-review` | `submitted` | `in-revision` | `published`.
    ///
    /// This only works for kinds that use status-change semantics.
    /// **Publications do NOT** — imbib dismisses a paper by moving it to the
    /// Dismissed library, and that path is imbib-core's own ops, not this one.
    /// impel tasks do not either: their state moves only through the kernel's
    /// `transition`.
    #[impress_method]
    #[impress_example(
        name = "missing-item",
        args = r#"{"id": "00000000-0000-0000-0000-000000000000", "status": "archived"}"#,
        expect = r#"{"ok":false}"#
    )]
    async fn set_status(&self, id: String, status: Option<String>) -> TriageResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Store-backed `TriageService`. `new()` uses the process store
/// (`koine_store::process::acquire_or_stand_in`); `with_store` takes an
/// explicit one over the port, as the tests do.
#[derive(Clone, Default)]
pub struct DefaultTriageService {
    store: Option<Arc<dyn AttributedStore>>,
}

impl DefaultTriageService {
    pub fn new() -> Self {
        Self { store: None }
    }

    pub fn with_store(store: Arc<dyn AttributedStore>) -> Self {
        Self { store: Some(store) }
    }

    fn store(&self) -> Arc<dyn AttributedStore> {
        self.store
            .clone()
            .unwrap_or_else(|| koine_store::process::acquire_or_stand_in().0)
    }
}

fn result(id: String, outcome: Result<(), StoreError>, done: String) -> TriageResult {
    match outcome {
        Ok(()) => TriageResult {
            ok: true,
            id,
            message: done,
        },
        Err(e) => TriageResult {
            ok: false,
            id,
            message: e.to_string(),
        },
    }
}

#[async_trait::async_trait]
impl TriageService for DefaultTriageService {
    async fn set_starred(&self, id: String, starred: bool) -> TriageResult {
        let outcome = triage_ops::set_starred(&*self.store(), &id, starred);
        let done = if starred { "Starred" } else { "Unstarred" };
        result(id.clone(), outcome, format!("{done} {id}."))
    }

    async fn set_flag(&self, id: String, color: Option<String>) -> TriageResult {
        let outcome = triage_ops::set_flag(&*self.store(), &id, color.as_deref());
        let done = match color.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => format!("Flagged {id} {c}."),
            None => format!("Cleared the flag on {id}."),
        };
        result(id, outcome, done)
    }

    async fn add_tag(&self, id: String, tag: String) -> TriageResult {
        let outcome = triage_ops::add_tag(&*self.store(), &id, &tag);
        result(id.clone(), outcome, format!("Tagged {id} '{tag}'."))
    }

    async fn remove_tag(&self, id: String, tag: String) -> TriageResult {
        let outcome = triage_ops::remove_tag(&*self.store(), &id, &tag);
        result(id.clone(), outcome, format!("Removed '{tag}' from {id}."))
    }

    async fn set_status(&self, id: String, status: Option<String>) -> TriageResult {
        let outcome = triage_ops::set_status(&*self.store(), &id, status.as_deref());
        let done = match status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => format!("Status of {id} is now '{s}'."),
            None => format!("Cleared the status of {id}."),
        };
        result(id, outcome, done)
    }
}

impress_service_impl! {
    service = TriageService,
    safety = mutating,
    since = "0.1.0",
    effects = {
        reads: [target(id)],
        writes: [target(id)],
        reach: [],
    },
    impl = DefaultTriageService,
    instance = DefaultTriageService::new,
    methods = [
        set_starred(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
            /// True to star it, false to unstar.
            starred: bool,
        ) -> TriageResult,
        set_flag(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
            /// A free-form color name ("red", "orange", "blue", …); `None`
            /// clears the flag.
            color: Option<String>,
        ) -> TriageResult,
        add_tag(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
            /// A hierarchical, slash-separated tag path ("reading/queue").
            tag: String,
        ) -> TriageResult,
        remove_tag(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
            /// The tag path to remove; a no-op if the item does not carry it.
            tag: String,
        ) -> TriageResult,
        set_status(
            /// The item's id, a lowercase UUID string, of any record kind.
            id: String,
            /// The lifecycle status string ("dismissed", "archived", or a
            /// schema-owned value); `None` clears it. Does not work for
            /// publications — see the trait doc.
            status: Option<String>,
        ) -> TriageResult,
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::tests::make_item;
    use impress_item::Item;
    use std::future::Future;

    fn item(store: &Arc<dyn AttributedStore>, id: &str) -> Item {
        store
            .get(uuid::Uuid::parse_str(id).unwrap())
            .unwrap()
            .expect("item present")
    }

    /// Run `case` once over each backend: the service behaves the same.
    async fn on_each_backend<F, Fut>(case: F)
    where
        F: Fn(Arc<dyn AttributedStore>) -> Fut,
        Fut: Future<Output = ()>,
    {
        case(Arc::new(koine_store_memory::MemoryStore::new())).await;
        let dir = tempfile::tempdir().expect("tempdir");
        case(Arc::new(
            koine_store_dir::DirStore::open(dir.path()).expect("open dir store"),
        ))
        .await;
    }

    #[tokio::test]
    async fn star_round_trip() {
        on_each_backend(|store| async move {
            let s = DefaultTriageService::with_store(store.clone());
            let id = make_item(&*store, "manuscript");

            let on = s.set_starred(id.clone(), true).await;
            assert!(on.ok, "{}", on.message);
            assert_eq!(on.id, id);
            assert!(item(&store, &id).is_starred);

            assert!(s.set_starred(id.clone(), false).await.ok);
            assert!(!item(&store, &id).is_starred);
        })
        .await;
    }

    #[tokio::test]
    async fn flag_set_and_cleared() {
        on_each_backend(|store| async move {
            let s = DefaultTriageService::with_store(store.clone());
            let id = make_item(&*store, "figure");

            assert!(s.set_flag(id.clone(), Some("red".into())).await.ok);
            assert_eq!(
                item(&store, &id).flag.map(|f| f.color),
                Some("red".to_string())
            );

            let cleared = s.set_flag(id.clone(), None).await;
            assert!(cleared.ok);
            assert!(cleared.message.contains("Cleared"));
            assert!(item(&store, &id).flag.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn tags_added_and_removed() {
        on_each_backend(|store| async move {
            let s = DefaultTriageService::with_store(store.clone());
            let id = make_item(&*store, "email-message");

            assert!(s.add_tag(id.clone(), "reading/queue".into()).await.ok);
            assert!(item(&store, &id).tags.contains(&"reading/queue".into()));

            // Idempotent add.
            assert!(s.add_tag(id.clone(), "reading/queue".into()).await.ok);
            assert_eq!(item(&store, &id).tags.len(), 1);

            assert!(s.remove_tag(id.clone(), "reading/queue".into()).await.ok);
            assert!(item(&store, &id).tags.is_empty());
            // Removing an absent tag is a no-op, not a failure.
            assert!(s.remove_tag(id.clone(), "never/applied".into()).await.ok);
        })
        .await;
    }

    #[tokio::test]
    async fn status_uses_the_reserved_values_and_free_text_alike() {
        on_each_backend(|store| async move {
            let s = DefaultTriageService::with_store(store.clone());
            let id = make_item(&*store, "manuscript");

            for value in [
                triage_ops::STATUS_DISMISSED,
                triage_ops::STATUS_ARCHIVED,
                "in-revision",
            ] {
                let set = s.set_status(id.clone(), Some(value.into())).await;
                assert!(set.ok, "{}", set.message);
                assert_eq!(
                    triage_ops::status(&*store, &id).unwrap().as_deref(),
                    Some(value)
                );
            }

            let cleared = s.set_status(id.clone(), None).await;
            assert!(cleared.ok);
            assert_eq!(triage_ops::status(&*store, &id).unwrap(), None);
        })
        .await;
    }

    /// The whole point of a generic triage service: one tool, every kind.
    #[tokio::test]
    async fn every_record_kind_can_be_triaged() {
        on_each_backend(|store| async move {
            let s = DefaultTriageService::with_store(store.clone());
            for schema in [
                "imbib/bibliography-entry",
                "manuscript",
                "figure",
                "email-message",
                "task",
                "agent-run",
            ] {
                let id = make_item(&*store, schema);
                assert!(s.set_starred(id.clone(), true).await.ok, "{schema} star");
                assert!(
                    s.set_flag(id.clone(), Some("blue".into())).await.ok,
                    "{schema} flag"
                );
                assert!(
                    s.add_tag(id.clone(), "triage".into()).await.ok,
                    "{schema} tag"
                );
                let it = item(&store, &id);
                assert!(it.is_starred && it.flag.is_some() && !it.tags.is_empty());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn bad_ids_fail_loudly_rather_than_silently() {
        on_each_backend(|store| async move {
            let s = DefaultTriageService::with_store(store.clone());

            let malformed = s.set_starred("not-a-uuid".into(), true).await;
            assert!(!malformed.ok);
            assert!(
                malformed.message.contains("invalid UUID"),
                "{}",
                malformed.message
            );

            let missing = uuid::Uuid::new_v4().to_string();
            let absent = s.add_tag(missing.clone(), "x".into()).await;
            assert!(!absent.ok);
            assert!(absent.message.contains("not found"), "{}", absent.message);

            let empty_tag = s
                .add_tag(make_item(&*store, "manuscript"), "  ".into())
                .await;
            assert!(!empty_tag.ok);
        })
        .await;
    }

    /// Without `with_store` the service reads the process store.
    #[tokio::test]
    async fn the_default_deployment_uses_the_process_store() {
        let (store, _) = koine_store::process::acquire_or_stand_in();
        let id = make_item(&*store, "manuscript");
        let s = DefaultTriageService::new();
        assert!(s.set_starred(id.clone(), true).await.ok);
        assert!(item(&store, &id).is_starred);
    }
}
