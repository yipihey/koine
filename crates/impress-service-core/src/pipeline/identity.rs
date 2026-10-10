//! Who is calling (ADR-0034 D3).
//!
//! A caller's identity is a fact the transport established — a UniFFI call
//! from the app is the person; MCP, the CLI and impel's tool loop are
//! agents; a daemon is the system — never a request field. The `actor`
//! argument the layout verbs still take is a claim the pipeline ignores
//! (plan-verb-pipeline D-P2): `impress_layout_service::authorship::actor_from`
//! derives the actor from [`super::context::current`] and warns when the
//! argument disagrees.

use std::fmt;

/// The identity of a caller, as the entry path established it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerIdentity {
    /// The person, through the app's own UI (a UniFFI call from Swift).
    Person,
    /// A named agent: `mcp`, `cli`, `impel`, `impress-ai`, `http`, `python`.
    Agent(String),
    /// An app calling with its own token (the app-side route).
    App(String),
    /// A registered runtime provider (ADR-0034 D7; P8).
    Provider(String),
    /// A daemon or a scheduler acting for nobody in particular.
    System(String),
}

impl CallerIdentity {
    pub fn agent(name: impl Into<String>) -> Self {
        CallerIdentity::Agent(name.into())
    }

    pub fn system(name: impl Into<String>) -> Self {
        CallerIdentity::System(name.into())
    }

    /// `human` | `agent` | `app` | `provider` | `system`.
    pub const fn kind(&self) -> &'static str {
        match self {
            CallerIdentity::Person => "human",
            CallerIdentity::Agent(_) => "agent",
            CallerIdentity::App(_) => "app",
            CallerIdentity::Provider(_) => "provider",
            CallerIdentity::System(_) => "system",
        }
    }

    /// The name, when the kind has one.
    pub fn name(&self) -> Option<&str> {
        match self {
            CallerIdentity::Person => None,
            CallerIdentity::Agent(n)
            | CallerIdentity::App(n)
            | CallerIdentity::Provider(n)
            | CallerIdentity::System(n) => Some(n),
        }
    }

    /// Whether this caller is the person or the app acting for them —
    /// the callers a review policy never queues.
    pub const fn is_person(&self) -> bool {
        matches!(self, CallerIdentity::Person | CallerIdentity::App(_))
    }

    /// The `author` string the audit row and the layout store carry:
    /// `human`, `agent:mcp`, `system:impel-taskd`.
    pub fn author(&self) -> String {
        match self.name() {
            Some(name) => format!("{}:{name}", self.kind()),
            None => self.kind().to_string(),
        }
    }

    /// The row's `{kind, name}` object.
    pub fn to_json(&self) -> serde_json::Value {
        match self.name() {
            Some(name) => serde_json::json!({ "kind": self.kind(), "name": name }),
            None => serde_json::json!({ "kind": self.kind() }),
        }
    }
}

impl fmt::Display for CallerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.author())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_author_string_names_the_kind_and_the_name() {
        assert_eq!(CallerIdentity::Person.author(), "human");
        assert_eq!(CallerIdentity::agent("mcp").author(), "agent:mcp");
        assert_eq!(
            CallerIdentity::system("impel-taskd").author(),
            "system:impel-taskd"
        );
        assert_eq!(CallerIdentity::App("imbib".into()).author(), "app:imbib");
        assert_eq!(CallerIdentity::Provider("julia".into()).kind(), "provider");
    }

    #[test]
    fn only_the_person_and_the_app_are_the_person() {
        assert!(CallerIdentity::Person.is_person());
        assert!(CallerIdentity::App("impress".into()).is_person());
        assert!(!CallerIdentity::agent("mcp").is_person());
        assert!(!CallerIdentity::system("d").is_person());
    }
}
