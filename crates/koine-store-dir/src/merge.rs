//! Conflict resolution (`docs/plan-koine-followups.md` F-D5): what the store
//! does with a `<id>.conflict-<n>.json` copy a file-sync tool left beside the
//! file it could not reconcile.
//!
//! [`MergePolicy::Report`] (the default) does nothing, as D4.4 always said.
//! Any other policy has [`DirStore::resolve_conflicts`] write a winner as
//! `<id>.json`, file the copy under `<root>/conflicts/` (kept: a merge is
//! reversible by hand), and record a `core/operation` row of type
//! `Custom("merge-conflict", {policy, file_clock, copy_clock, copy})` on the
//! item, so `operations_for` shows when and how it was merged.
//!
//! The winner, the operation row and the clock are one journaled write; the
//! copy is moved after it. A crash between leaves a copy whose clock is below
//! the new file's, which the next resolution files as stale. Conflict copies
//! of *operation* rows are left reported: an operation is immutable, and there
//! is nothing to merge.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

/// How to combine a file with a conflict copy of the same item.
pub trait Merge: Send + Sync {
    /// The merged item. The store sets the result's `id` and `schema` to the
    /// file's, gives it a fresh `logical_clock` that sorts after both inputs,
    /// and raises `modified` to the later of the two.
    fn merge(&self, file: &Item, copy: &Item) -> Item;
}

/// What a store does with a conflict copy that has a higher clock than its file.
#[derive(Clone, Default)]
pub enum MergePolicy {
    /// Report it ([`DirStore::conflicts`]) and leave both files alone.
    #[default]
    Report,
    /// The copy becomes the file.
    TakeCopy,
    /// The file stays (with a fresh clock, so the copy is behind it); the copy
    /// is filed away.
    KeepFile,
    /// [`union_merge`]: the policy for two roots that each added things.
    Union,
    /// The app's own rule.
    Custom(Arc<dyn Merge>),
}

impl fmt::Debug for MergePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MergePolicy::Report => f.write_str("Report"),
            MergePolicy::TakeCopy => f.write_str("TakeCopy"),
            MergePolicy::KeepFile => f.write_str("KeepFile"),
            MergePolicy::Union => f.write_str("Union"),
            MergePolicy::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

impl MergePolicy {
    /// The name recorded in the `merge-conflict` operation and in [`Resolved`].
    pub fn name(&self) -> &'static str {
        match self {
            MergePolicy::Report => "report",
            MergePolicy::TakeCopy => "take-copy",
            MergePolicy::KeepFile => "keep-file",
            MergePolicy::Union => "union",
            MergePolicy::Custom(_) => "custom",
        }
    }
}

/// The union of two states of one item: tags and references are unioned,
/// payload keys are unioned with the higher-clocked side winning a key both
/// carry with different values, and the flag, read, starred, priority,
/// visibility and parent (and every other envelope field) come from the
/// higher clock (the file when the clocks tie). `modified` is the later; the
/// clock is the higher of the two (the store stamps a fresh one when it
/// resolves).
pub fn union_merge(file: &Item, copy: &Item) -> Item {
    let (high, low) = if copy.logical_clock > file.logical_clock {
        (copy, file)
    } else {
        (file, copy)
    };
    let mut out = high.clone();
    for (key, value) in &low.payload {
        out.payload
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    for tag in &low.tags {
        if !out.tags.contains(tag) {
            out.tags.push(tag.clone());
        }
    }
    out.tags.sort();
    out.tags.dedup();
    for r in &low.references {
        if !out
            .references
            .iter()
            .any(|e| e.target == r.target && e.edge_type == r.edge_type)
        {
            out.references.push(r.clone());
        }
    }
    out.references.sort_by_key(reference_key);
    out.modified = file.modified.max(copy.modified);
    out.logical_clock = file.logical_clock.max(copy.logical_clock);
    out
}

/// One conflict [`DirStore::resolve_conflicts`] closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub id: ItemId,
    /// [`MergePolicy::name`].
    pub policy: &'static str,
    /// The file's clock before; `None` when the file was missing.
    pub file_clock: Option<u64>,
    pub copy_clock: u64,
    /// Where the copy went: `<root>/conflicts/<id>.conflict-<n>.json`.
    pub moved_copy: PathBuf,
}

impl DirStore {
    /// Set the [`MergePolicy`]. A policy other than `Report` resolves the
    /// conflicts already pending at once; if that fails (the disk refuses),
    /// they stay reported and [`DirStore::resolve_conflicts`] says why when
    /// called.
    pub fn with_merge_policy(mut self, policy: MergePolicy) -> Self {
        self.merge_policy = policy;
        if !matches!(self.merge_policy, MergePolicy::Report) {
            let _ = self.resolve_conflicts();
        }
        self
    }

    pub fn merge_policy(&self) -> &MergePolicy {
        &self.merge_policy
    }

    /// Apply the [`MergePolicy`] to every conflict copy on disk, under the
    /// writer lock. Returns the conflicts it closed (a stale copy, one at or
    /// below its file's clock, is filed under `conflicts/` quietly and is not
    /// listed); [`DirStore::conflicts`] holds what remains. Under `Report`
    /// nothing is touched and the list is empty.
    pub fn resolve_conflicts(&self) -> Result<Vec<Resolved>, StoreError> {
        if matches!(self.merge_policy, MergePolicy::Report) {
            return Ok(Vec::new());
        }
        let mut tx = self.begin()?;
        let operations_dir = self.root.join("operations");
        let conflicts_dir = self.root.join("conflicts");
        let mut resolved = Vec::new();
        let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut taken: Vec<PathBuf> = Vec::new();
        let mut destination = |copy: &Path| -> PathBuf {
            let name = copy
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut dest = conflicts_dir.join(&name);
            let stem = name.strip_suffix(".json").unwrap_or(&name).to_owned();
            let mut n = 1;
            while dest.exists() || taken.contains(&dest) {
                dest = conflicts_dir.join(format!("{stem}.dup-{n}.json"));
                n += 1;
            }
            taken.push(dest.clone());
            dest
        };

        for c in self.scan_copies()? {
            if c.file.parent() == Some(operations_dir.as_path()) {
                continue;
            }
            let Some(copy_row) = Self::read_row_at(&c.copy, false)? else {
                continue;
            };
            let copy = copy_row.item;
            let id = c.id;
            tx.load(id)?;
            let file = tx.row(id).map(|r| r.item.clone());

            if let Some(f) = &file {
                if copy.logical_clock <= f.logical_clock {
                    moves.push((c.copy.clone(), destination(&c.copy)));
                    continue;
                }
            }

            let policy = &self.merge_policy;
            let moved_copy = destination(&c.copy);
            let winner = match (&file, policy) {
                (_, MergePolicy::Report) => continue,
                (None, MergePolicy::KeepFile) => {
                    // Nothing to keep: the copy is filed away.
                    resolved.push(Resolved {
                        id,
                        policy: policy.name(),
                        file_clock: None,
                        copy_clock: copy.logical_clock,
                        moved_copy: moved_copy.clone(),
                    });
                    moves.push((c.copy.clone(), moved_copy));
                    continue;
                }
                (None, _) | (Some(_), MergePolicy::TakeCopy) => copy.clone(),
                (Some(f), MergePolicy::KeepFile) => f.clone(),
                (Some(f), MergePolicy::Union) => union_merge(f, &copy),
                (Some(f), MergePolicy::Custom(m)) => m.merge(f, &copy),
            };
            let mut winner = winner;

            tx.hlc.observe(copy.logical_clock);
            if let Some(f) = &file {
                tx.hlc.observe(f.logical_clock);
            }
            let clock = tx.hlc.next();
            let base_schema = file.as_ref().map_or(&copy.schema, |f| &f.schema).clone();
            winner.id = id;
            winner.schema = base_schema;
            winner.logical_clock = clock;
            if let Some(f) = &file {
                winner.modified = f.modified.max(copy.modified);
            }
            let schema = winner.schema.to_string();
            tx.rows.insert(
                id,
                Some(Row {
                    item: winner.clone(),
                    op: None,
                }),
            );
            tx.mark_dirty(id);

            if file.is_some() {
                let relative = c
                    .copy
                    .strip_prefix(&self.root)
                    .map(|p| {
                        p.components()
                            .map(|s| s.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .unwrap_or_else(|_| c.copy.display().to_string());
                let mut data = BTreeMap::new();
                data.insert("policy".to_owned(), Value::String(policy.name().into()));
                data.insert(
                    "file_clock".to_owned(),
                    file.as_ref()
                        .map_or(Value::Null, |f| Value::Int(f.logical_clock as i64)),
                );
                data.insert(
                    "copy_clock".to_owned(),
                    Value::Int(copy.logical_clock as i64),
                );
                data.insert("copy".to_owned(), Value::String(relative));
                let spec = OperationSpec {
                    target_id: id,
                    op_type: OperationType::Custom("merge-conflict".into(), Value::Object(data)),
                    intent: OperationIntent::Editorial,
                    reason: Some(format!("sync conflict merged by {}", policy.name())),
                    batch_id: None,
                    author: self.author.clone(),
                    author_kind: self.author_kind,
                    retention: RetentionTier::Durable,
                };
                let operation_id = Uuid::new_v4();
                let op_row = operation_row(
                    self,
                    &spec,
                    &schema,
                    clock,
                    trunc_ms(Utc::now()),
                    operation_id,
                    None,
                );
                tx.rows.insert(operation_id, Some(op_row));
                tx.mark_dirty(operation_id);
            }

            let event = match &file {
                Some(old) => ItemEvent::Updated {
                    id,
                    mutations: diff_items(old, &winner),
                },
                None => ItemEvent::Created(Box::new(winner)),
            };
            tx.events.push((Some(schema), event));
            resolved.push(Resolved {
                id,
                policy: policy.name(),
                file_clock: file.map(|f| f.logical_clock),
                copy_clock: copy.logical_clock,
                moved_copy: moved_copy.clone(),
            });
            moves.push((c.copy.clone(), moved_copy));
        }

        let (plan, events) = tx.plan()?;
        self.commit_plan(&plan)?;
        if !moves.is_empty() {
            std::fs::create_dir_all(&conflicts_dir).map_err(|e| storage("create conflicts/", e))?;
        }
        for (from, to) in &moves {
            match std::fs::rename(from, to) {
                Ok(()) => {}
                // Another handle filed it first.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(storage(
                        format!("move {} to {}", from.display(), to.display()),
                        e,
                    ))
                }
            }
        }
        *self.conflicts.lock().map_err(poisoned)? = self.scan_conflicts()?;
        drop(tx);
        self.emit_all(events);
        Ok(resolved)
    }
}
