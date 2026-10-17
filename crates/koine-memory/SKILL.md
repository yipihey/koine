---
name: memory
description: Remember and recall what matters across sessions — claims, episodes and standing instructions — with the memory-service verbs.
---

# Memory

Memory is three kinds of item: a `claim` (a fact or decision), an `episode` (what happened in a session) and an `instruction` (a standing rule that binds regardless of topic). Seven verbs act on them:

- `memory-service_recall` `{query, subject_ref, limit, include_superseded}`: memories matching the words in `query`, best first; an empty `query` returns the most recent current memories. `subject_ref` narrows to memories about one item (a lowercase UUID). Recall before you ask the person something they may already have told you.
- `memory-service_memory-brief` `{topic, subject_ref, max_entries}`: the start-of-session shape: instructions first, then claims (on `topic` when given), then recent episodes.
- `memory-service_remember` `{kind, title, body, claim_type, confidence, subject_refs, evidence_refs}`: write one. A near-duplicate of an existing memory is *confirmed* (one memory with more evidence) rather than written twice; the result's `action` says which happened.
- `memory-service_confirm-claim` `{id}`: record that an existing memory held up again.
- `memory-service_supersede-claim` `{old_id, title, body, reason}`: correct a memory. The old one stays for the record but is no longer recalled.
- `memory-service_forget` `{id}`: withhold a memory from recall without deleting it.
- `memory-service_memory-status`: how many memories of each kind exist and how many are current, and whether the vector tier is running.

Each returns `ok` and a `message`; a bad id or a missing memory gives `ok: false` with the reason, not an error.

Remember what the person would otherwise have to repeat: preferences, decisions and their reasons, facts about their data. Keep a memory to one thing, and supersede it when it changes rather than writing a contradicting one.
