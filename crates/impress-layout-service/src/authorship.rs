//! Who wrote a row, and on which side of the ADR-0031 D7 line.
//!
//! The one copy of these, used by this crate's store and by
//! `impress-surface-service`'s (review RS-S21), which already depends on this
//! crate for `surface_show`'s composed verbs. They cannot live in
//! `impress-service-core` with the report types: they speak the store's
//! vocabulary (`ActorKind`, `RetentionTier`, `OperationIntent`), and that
//! crate is on the kit's pure tier, which may not reach `impress-core`
//! (docs/kit-manifest.md).

use impress_item::item::ActorKind;
use impress_item::operation::{OperationIntent, RetentionTier};
use impress_service_core::pipeline::CallerIdentity;

/// Which side of the ADR-0031 D7 line a write is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ephemerality {
    /// A gesture: arrangement, content, selection, a surface's state.
    /// Coalesced and compacted.
    Exploration,
    /// A commit: a saved layout, a materialized binding, a surface's spec.
    /// Kept forever.
    Commit,
}

impl Ephemerality {
    pub fn retention(self) -> RetentionTier {
        match self {
            Ephemerality::Exploration => RetentionTier::Ephemeral,
            Ephemerality::Commit => RetentionTier::Durable,
        }
    }

    pub fn intent(self) -> OperationIntent {
        match self {
            Ephemerality::Exploration => OperationIntent::Routine,
            // A commit is a decision about how work is arranged, which is what
            // `Editorial` means in the ADR-0003 vocabulary.
            Ephemerality::Commit => OperationIntent::Editorial,
        }
    }
}

/// The author string a service writes with an operation: `IMPRESS_AUTHOR`
/// when set, so a host that knows who the user is can say so; otherwise
/// `<actor>:<service>` (`human:layout-service`, `agent:surface-service`).
/// The actor kind is the whole of what is known, and saying that plainly
/// beats inventing an identity.
pub fn author_for_service(actor: ActorKind, service: &str) -> String {
    if let Ok(author) = std::env::var("IMPRESS_AUTHOR") {
        let author = author.trim();
        if !author.is_empty() {
            return author.to_string();
        }
    }
    let kind = match actor {
        ActorKind::Human => "human",
        ActorKind::Agent => "agent",
        ActorKind::System => "system",
    };
    format!("{kind}:{service}")
}

/// The actor a verb records: derived from the caller identity the pipeline
/// established (ADR-0034 D3, plan-verb-pipeline D-P2), never from the
/// `actor` argument.
///
/// Under a pipeline call context the argument is **ignored**: the person
/// is whoever the transport said the person is, and an `actor: "human"`
/// sent over MCP or HTTP by an agent is recorded as the agent it came from
/// (PL-3), with a warning naming the claim. The argument stays on the 24
/// layout verbs until P3 removes it. Outside a call context — a direct
/// Rust call from a test or a host — the argument is still read the way it
/// always was: `None` means [`ActorKind::Agent`], because a caller that
/// forgets to say who it is must not be recorded as the user.
pub fn actor_from(raw: Option<&str>) -> ActorKind {
    let claimed = claimed_actor(raw);
    match impress_service_core::pipeline::context::current() {
        Some(context) => {
            let derived = actor_kind_of(&context.caller);
            if raw.is_some_and(|r| !r.trim().is_empty()) && claimed != derived {
                tracing::warn!(
                    target: "layout",
                    "{}: actor argument {raw:?} ignored — the caller is {} (ADR-0034 D3; \
                     the argument is derived since P2 and removed in P3)",
                    context.verb,
                    context.caller,
                );
            }
            derived
        }
        None => claimed,
    }
}

/// The `ActorKind` a caller identity records as: the person is human, the
/// system is the system, and every agent, app route and provider is an
/// agent — an app's HTTP route is not the person at the keyboard.
///
/// The same mapping `impress_core::call_context::actor_kind_of` makes for the
/// store's own audit rows; a copy here because both of its types are the
/// kit's (`CallerIdentity` is the pipeline's, `ActorKind` the store port's)
/// and this crate must not reach `impress-core` for a `match`.
fn actor_kind_of(caller: &CallerIdentity) -> ActorKind {
    match caller {
        CallerIdentity::Person => ActorKind::Human,
        CallerIdentity::System(_) => ActorKind::System,
        CallerIdentity::Agent(_) | CallerIdentity::App(_) | CallerIdentity::Provider(_) => {
            ActorKind::Agent
        }
    }
}

/// The `actor` argument as written: `human` | `user` | `person`, `system`,
/// anything else (or nothing) an agent.
pub fn claimed_actor(raw: Option<&str>) -> ActorKind {
    match raw.map(|a| a.trim().to_ascii_lowercase()).as_deref() {
        Some("human") | Some("user") | Some("person") => ActorKind::Human,
        Some("system") => ActorKind::System,
        _ => ActorKind::Agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings every row already written carries; a change here would
    /// split one author into two in history.
    #[test]
    fn the_author_names_the_actor_and_the_service() {
        if std::env::var("IMPRESS_AUTHOR").is_ok() {
            return;
        }
        assert_eq!(
            author_for_service(ActorKind::Human, "layout-service"),
            "human:layout-service"
        );
        assert_eq!(
            author_for_service(ActorKind::Agent, "surface-service"),
            "agent:surface-service"
        );
        assert_eq!(
            author_for_service(ActorKind::System, "layout-service"),
            "system:layout-service"
        );
    }

    #[test]
    fn a_missing_or_unknown_actor_is_an_agent() {
        assert_eq!(actor_from(None), ActorKind::Agent);
        assert_eq!(actor_from(Some("robot")), ActorKind::Agent);
        assert_eq!(actor_from(Some(" User ")), ActorKind::Human);
        assert_eq!(actor_from(Some("system")), ActorKind::System);
    }

    /// PL-3, D-P2: under a call the argument is a claim the pipeline
    /// ignores. An agent saying `human` is an agent; the person saying
    /// nothing is the person.
    #[test]
    fn under_a_call_the_actor_is_the_caller_not_the_argument() {
        use impress_service_core::pipeline::context::{sync_scope, CallContext};
        use impress_service_core::pipeline::CallerIdentity;
        use std::sync::Arc;
        let context = |caller: CallerIdentity| {
            Arc::new(CallContext {
                call_id: "c".into(),
                trace_id: "t".into(),
                parent_call: None,
                caller,
                verb: "layout-service_split".into(),
                store_override: None,
                mutation_ids: Default::default(),
                adverbs: Default::default(),
                inverse: Default::default(),
            })
        };
        sync_scope(context(CallerIdentity::agent("mcp")), || {
            assert_eq!(actor_from(Some("human")), ActorKind::Agent);
            assert_eq!(actor_from(None), ActorKind::Agent);
        });
        sync_scope(context(CallerIdentity::Person), || {
            assert_eq!(actor_from(None), ActorKind::Human);
            assert_eq!(actor_from(Some("agent")), ActorKind::Human);
        });
        sync_scope(context(CallerIdentity::system("impel-taskd")), || {
            assert_eq!(actor_from(Some("human")), ActorKind::System);
        });
    }
}
