---
name: collections
description: Organise any items into nested collections (folders) with the collection-service verbs; list, create, rename, move, reorder, delete, and file items in them.
---

# Collections

A collection is a named, ordered, nestable grouping of items of any kind. Twelve verbs act on one hierarchy, chosen by `binding`: `generic` is the mixed-kind `collection@1.0.0` schema; an application may register more (impress registers `imbib`, `manuscript` and `figure`). An unknown binding is refused with the list of the ones that exist.

- `collection-service_tree` `{binding}`: every collection in the hierarchy, flat, ordered by `sort_order`. Start here: every other verb takes ids this returns. Build the tree from each row's `parent_id` (`null` is a root).
- `collection-service_create` `{binding, name, parent_id, kind_scope}`: a new collection under `parent_id` (`null` for a root). `kind_scope` (generic only) names the record kind it organises, `any` by default.
- `collection-service_rename` `{binding, id, name}`.
- `collection-service_reparent` `{binding, id, new_parent_id}`: move under another collection, or to the root with `null`. A move under the collection's own descendant is refused (cycle).
- `collection-service_reorder` `{binding, id, sort_order}`: lower sorts first among siblings.
- `collection-service_delete` `{binding, id}`: destructive for the collection only; its members survive.
- `collection-service_add-members` `{binding, collection_id, item_ids}`: file items in; idempotent; `applied` counts those newly filed.
- `collection-service_remove-members` `{binding, collection_id, item_ids}`: unfile items; the items themselves are untouched.
- `collection-service_member-counts` `{binding, collection_ids}`: counts aligned with the ids given; an unknown id counts 0.
- `collection-service_migration-status`, `collection-service_migrate` `{dry_run}` and `collection-service_rollback`: converge an application's legacy collection schemas onto `collection@1.0.0`. Where there are none, they say so and change nothing. Run `migrate` with `dry_run: true` first.

Each returns `ok` and a `message`; a bad id, a missing collection or a wrong-schema collection gives `ok: false` with the reason, not an error.

File what belongs together, name collections for the work they serve, and nest only as deep as you will browse.
