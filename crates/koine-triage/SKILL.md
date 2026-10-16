---
name: triage
description: Star, flag, tag and set the status of any item (publication, manuscript, figure, message, task) with the triage-service verbs.
---

# Triage

Five verbs act on any item by id (a lowercase UUID string), whatever app wrote it:

- `triage-service_set-starred` `{id, starred}`: star or unstar.
- `triage-service_set-flag` `{id, color}`: a free-form colour such as `red`; `null` clears the flag.
- `triage-service_add-tag` `{id, tag}`: add a slash-separated tag path such as `reading/queue`. Idempotent.
- `triage-service_remove-tag` `{id, tag}`: remove a tag; a tag the item lacks is a no-op.
- `triage-service_set-status` `{id, status}`: set the lifecycle status string, or clear it with `null`.

Each returns `{ok, id, message}`; a bad id or a missing item gives `ok: false` with the reason, not an error.

`status` is free text apart from two reserved values: `dismissed` (hidden from every scope but Dismissed; never destructive) and `archived` (a deliberate end state; restorable). Publications are the exception: imbib dismisses a paper by moving it to the Dismissed library, so writing `status` on a publication changes nothing. Tasks move only through their own transitions.

Triage before you delete: star what to keep, tag what to revisit, archive what is finished.
