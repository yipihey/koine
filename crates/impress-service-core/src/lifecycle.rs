//! The rename table (plan-verb-pipeline-and-transport.md P3, part B): one
//! place declaring every verb rename and view-kind rename the suite has ever
//! shipped, read by the rename passes in `impress-layout-service` (stored
//! layouts and presets) and `impress-surface-service` (stored surface
//! specs).
//!
//! # Why here
//!
//! Both consuming crates already depend on this one — it is the pure kit
//! crate the whole `#[impress_service]` pipeline is built from
//! (docs/kit-manifest.md) — and it is on the kit's **pure** tier, so putting
//! the table here rather than in `impress-layout` or `impress-surface` keeps
//! it reachable from *both* rename passes without either service crate
//! depending on the other's pure crate for a fact that belongs to neither
//! tree. `impress-core` was the other candidate; this crate is used instead
//! because a rename is a fact about **verb identity** (`descriptor.rs`'s
//! `since`/`Deprecation`/`Source::Alias`, ADR-0034 D5) and a view kind is the
//! one other closed vocabulary a verb argument names
//! (`impress_layout::ViewKindId::KNOWN`) — both belong with the descriptor,
//! not with the store.
//!
//! # The contract each rename pass follows
//!
//! * A **system-seeded** row that still matches what the suite shipped is
//!   rewritten unconditionally, the same as `upgrade_if_untouched`
//!   (`impress-layout-service/src/presets.rs`) upgrades a preset revision.
//! * A **user-edited** row is rewritten too — an old verb or view-kind name
//!   in a user's own surface or preset must still work — but nothing is
//!   thrown away: the store's own operation log already keeps the previous
//!   payload value (every write here goes through
//!   `SqliteItemStore::apply_operation`, exactly like every other write these
//!   two services make), and the pass logs the rewrite at `info`.
//! * The pass runs once per [`RenameTable::version`]: each store records the
//!   highest version it has applied (`SqliteItemStore::{get,set}_store_metadata`)
//!   and a pass whose version is not newer than that is a no-op.
//!
//! # H-P3-1 — the extension point
//!
//! docs/plan-self-reflective-layer.md's H-P3-1 says this pass must later also
//! visit workflow and scenario documents, whose record kinds do not exist
//! yet. Nothing for them is written here — see [`RenameVisitor`], the named
//! seam a future workflow/scenario pass implements without this module or
//! the table above changing shape.

use std::collections::BTreeSet;

use crate::descriptor_handle::VerbHandle;

/// A stored reference to a provider verb that its owner dropped without an
/// alias. Reports name the document and exact JSON pointer; they never edit
/// a stored document or synthesize a replacement verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecatedReference {
    pub document_kind: &'static str,
    pub document_id: String,
    pub pointer: String,
    pub verb: String,
}

/// An explicit inventory snapshot for read-only lifecycle reporting.
/// Linked deprecations and merely unavailable provider verbs are excluded.
#[derive(Debug, Clone, Default)]
pub struct DeprecatedProviderNames(BTreeSet<String>);

impl DeprecatedProviderNames {
    pub fn from_handles(handles: impl IntoIterator<Item = VerbHandle>) -> Self {
        Self(
            handles
                .into_iter()
                .filter_map(|handle| match handle {
                    VerbHandle::Provider(provider) if provider.deprecated_since.is_some() => {
                        Some(provider.name.clone())
                    }
                    _ => None,
                })
                .collect(),
        )
    }

    /// Explicit names for offline analysis and isolated rename-pass tests.
    /// Production callers should use [`Self::from_handles`] with the current
    /// validated registry inventory.
    pub fn from_names(names: impl IntoIterator<Item = String>) -> Self {
        Self(names.into_iter().collect())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One renamed identifier: `(old, new)`.
pub type Rename = (&'static str, &'static str);

/// Every verb rename and view-kind rename the suite has shipped, versioned so
/// a rename pass can tell whether it has already run.
///
/// **Append-only.** A row already in one of the two lists must never be
/// edited or removed — a store that applied version N must still be able to
/// answer "what did N rewrite?" from this table forever, the same append-only
/// discipline `SHIPPED_FINGERPRINTS` (`impress-layout-service/src/presets.rs`)
/// already holds. A new rename bumps [`RenameTable::version`] and appends to
/// one of `verbs` / `view_kinds`; it never mutates an existing entry.
#[derive(Debug, Clone, Copy)]
pub struct RenameTable {
    /// Bumped exactly when a rename is appended. The rename passes record the
    /// highest version they have applied and skip a table at or below it.
    pub version: u32,
    /// Verb renames: `(old_name, new_name)`, both the full callable name
    /// (`"service-kebab_verb-kebab"`).
    pub verbs: &'static [Rename],
    /// View-kind renames: `(old, new)`, both spellings from
    /// `impress_layout::ViewKindId`'s vocabulary.
    pub view_kinds: &'static [Rename],
}

impl RenameTable {
    /// True for a table with nothing to rewrite — what every rename pass's
    /// "nothing shipped yet" test asserts of [`SHIPPED_RENAMES`].
    pub const fn is_empty(&self) -> bool {
        self.verbs.is_empty() && self.view_kinds.is_empty()
    }

    /// The new name for `name`, when this table renamed it. `None` for a
    /// verb the table has never touched — not necessarily a verb that
    /// exists, this table has no opinion on that.
    pub fn rename_verb(&self, name: &str) -> Option<&'static str> {
        rename_of(self.verbs, name)
    }

    /// The new spelling for `kind`, when this table renamed it.
    pub fn rename_view_kind(&self, kind: &str) -> Option<&'static str> {
        rename_of(self.view_kinds, kind)
    }
}

fn rename_of(table: &[Rename], name: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(_, new)| *new)
}

/// The suite's shipped rename table. **Starts empty, on purpose** — P3b builds
/// the machinery; the first real rename (ADR-0024 D7's move of the four
/// legacy MCP tools under a real namespace) is P3a's to append, along with
/// every rename after it. Nothing here may name a real verb or view kind
/// until that append happens.
pub const SHIPPED_RENAMES: RenameTable = RenameTable {
    version: 0,
    verbs: &[],
    view_kinds: &[],
};

/// The named extension point plan-self-reflective-layer.md's H-P3-1 asks for:
/// one document kind the rename pass visits.
///
/// `impress-layout-service` (stored layouts and presets) and
/// `impress-surface-service` (stored surface specs) each provide one
/// implementation today. A later workflow or scenario document kind adds a
/// third implementation, in whichever crate owns that kind, once it exists —
/// nothing in this trait, [`RenameTable`], or the two existing passes needs
/// to change for that to happen. The trait is deliberately thin: each
/// document kind's own crate has its own store type and its own `Result`
/// (`impress-layout-service::Result` is a different type from
/// `impress-surface-service::Result`), so the actual rewrite is a plain
/// method on that crate's own pass type, named and documented like every
/// other visitor's `apply` — this trait exists only so every visitor can be
/// named and counted the same way, in a list, without a shared error type
/// forcing one on crates that have no other reason to agree on one.
pub trait RenameVisitor {
    /// The document kind this visitor rewrites, for logs and for the
    /// applied-version marker key (`"layout"`, `"surface"`, and later
    /// `"workflow"` / `"scenario"`).
    fn kind(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Safety, SafetyClass};
    use crate::provider::{ProviderStatus, ProviderVerb};
    use std::sync::Arc;

    #[test]
    fn the_shipped_table_starts_empty() {
        assert!(SHIPPED_RENAMES.is_empty());
        assert_eq!(SHIPPED_RENAMES.version, 0);
    }

    #[test]
    fn a_table_answers_only_for_names_it_renamed() {
        let table = RenameTable {
            version: 1,
            verbs: &[("old-service_a", "old-service_b")],
            view_kinds: &[("legacy-kind", "outline")],
        };
        assert_eq!(table.rename_verb("old-service_a"), Some("old-service_b"));
        assert_eq!(table.rename_verb("old-service_b"), None);
        assert_eq!(table.rename_verb("unrelated"), None);
        assert_eq!(table.rename_view_kind("legacy-kind"), Some("outline"));
        assert_eq!(table.rename_view_kind("outline"), None);
        assert!(!table.is_empty());
    }

    #[test]
    fn snapshot_includes_dropped_provider_but_not_merely_unavailable_one() {
        fn provider(name: &str, deprecated: Option<&str>) -> VerbHandle {
            let safety = Safety {
                class: SafetyClass::External,
                idempotent: false,
            };
            VerbHandle::Provider(Arc::new(ProviderVerb {
                name: name.into(),
                service: "julia-service".into(),
                method: "echo".into(),
                description: "fixture".into(),
                input_schema: serde_json::json!({"type":"object"}),
                output_schema: serde_json::json!({"type":"object"}),
                declared_safety: safety,
                effective_safety: safety,
                since: "1.0".into(),
                examples: Vec::new(),
                provider_id: "julia".into(),
                status: ProviderStatus::Unavailable,
                deprecated_since: deprecated.map(str::to_owned),
                generation: 1,
            }))
        }
        let names = DeprecatedProviderNames::from_handles([
            provider("julia-service_dropped", Some("1.1")),
            provider("julia-service_offline", None),
        ]);
        assert!(names.contains("julia-service_dropped"));
        assert!(!names.contains("julia-service_offline"));
    }
}
