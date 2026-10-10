//! A cloneable descriptor that can own runtime metadata without changing the
//! generated, static linked descriptors.

use std::borrow::Cow;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::descriptor::{Effects, ExampleTier, Reach, Safety, VerbDescriptor};
use crate::provider::{ProviderStatus, ProviderVerb};

const PROVIDER_EFFECTS: Effects = Effects {
    reads: &[],
    writes: &[],
    reach: &[Reach::Provider],
};

#[derive(Debug, Clone)]
pub enum VerbHandle {
    Linked(&'static VerbDescriptor),
    Provider(Arc<ProviderVerb>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleSource<'a> {
    Linked,
    Provider(&'a str),
}

#[derive(Debug, Clone, Copy)]
pub struct ExampleView<'a> {
    pub name: &'a str,
    pub args: &'a str,
    pub expect: Option<&'a str>,
    pub tier: ExampleTier,
}

impl ExampleView<'_> {
    /// Parse the declared argument object as static `Example::args_value` does.
    pub fn args_value(&self) -> Value {
        serde_json::from_str(self.args).unwrap_or(Value::Null)
    }
}

impl VerbHandle {
    pub fn linked(&self) -> Option<&'static VerbDescriptor> {
        match self {
            Self::Linked(verb) => Some(verb),
            Self::Provider(_) => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.name,
            Self::Provider(verb) => &verb.name,
        }
    }

    pub fn service(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.service,
            Self::Provider(verb) => &verb.service,
        }
    }

    pub fn method(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.method,
            Self::Provider(verb) => &verb.method,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.description,
            Self::Provider(verb) => &verb.description,
        }
    }

    pub fn since(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.since,
            Self::Provider(verb) => &verb.since,
        }
    }

    pub fn safety(&self) -> Safety {
        match self {
            Self::Linked(verb) => verb.safety,
            Self::Provider(verb) => verb.effective_safety,
        }
    }

    pub fn mcp_annotations(&self) -> Value {
        self.safety().mcp_annotations()
    }

    pub fn effects(&self) -> Effects {
        match self {
            Self::Linked(verb) => verb.effects,
            Self::Provider(_) => PROVIDER_EFFECTS,
        }
    }

    pub fn strict(&self) -> bool {
        match self {
            Self::Linked(verb) => verb.strict,
            Self::Provider(_) => true,
        }
    }

    pub fn budget_ms(&self) -> Option<u64> {
        match self {
            Self::Linked(verb) => verb.budget_ms,
            Self::Provider(_) => None,
        }
    }

    pub fn replay_full(&self) -> bool {
        match self {
            Self::Linked(verb) => verb.replay_full,
            Self::Provider(_) => false,
        }
    }

    pub fn source(&self) -> HandleSource<'_> {
        match self {
            Self::Linked(_) => HandleSource::Linked,
            Self::Provider(verb) => HandleSource::Provider(&verb.provider_id),
        }
    }

    /// The linked schema is generated on demand here; the pipeline's
    /// existing static cache can still be used through [`Self::linked`].
    pub fn input_schema(&self) -> Cow<'_, Value> {
        match self {
            Self::Linked(verb) => Cow::Owned((verb.input_schema)()),
            Self::Provider(verb) => Cow::Borrowed(&verb.input_schema),
        }
    }

    pub fn output_schema(&self) -> Cow<'_, Value> {
        match self {
            Self::Linked(verb) => Cow::Owned((verb.output_schema)()),
            Self::Provider(verb) => Cow::Borrowed(&verb.output_schema),
        }
    }

    pub fn examples(&self) -> Vec<ExampleView<'_>> {
        match self {
            Self::Linked(verb) => verb
                .examples
                .iter()
                .map(|example| ExampleView {
                    name: example.name,
                    args: example.args,
                    expect: example.expect,
                    tier: example.tier,
                })
                .collect(),
            Self::Provider(verb) => verb
                .examples
                .iter()
                .map(|example| ExampleView {
                    name: &example.name,
                    args: &example.args,
                    expect: example.expect.as_deref(),
                    tier: ExampleTier::B,
                })
                .collect(),
        }
    }

    pub fn has_alias(&self, name: &str) -> bool {
        self.linked().is_some_and(|verb| verb.has_alias(name))
    }

    pub fn deprecation_notice(&self) -> Option<Value> {
        match self {
            Self::Linked(verb) => verb.deprecation_notice(),
            Self::Provider(verb) => verb.deprecated_since.as_ref().map(|since| {
                json!({
                    "since": since,
                    "use": verb.name,
                    "note": "provider no longer advertises this verb",
                })
            }),
        }
    }

    pub fn alias_deprecation_notice(&self) -> Option<Value> {
        self.linked()
            .and_then(VerbDescriptor::alias_deprecation_notice)
    }

    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Linked(_) => None,
            Self::Provider(verb) => Some(&verb.provider_id),
        }
    }

    pub fn provider_status(&self) -> Option<ProviderStatus> {
        match self {
            Self::Linked(_) => None,
            Self::Provider(verb) => Some(verb.status),
        }
    }

    pub fn generation(&self) -> Option<u64> {
        match self {
            Self::Linked(_) => None,
            Self::Provider(verb) => Some(verb.generation),
        }
    }
}
